use crate::{Result, ReviewEvent, ReviewState, Store};
use fsrs::{FSRSItem, FSRSReview, MemoryState, FSRS};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

const SECONDS_PER_DAY: i64 = 86_400;
const RETENTION: f32 = 0.9;
const AGAIN_SECONDS: i64 = 60;
pub(crate) const MAX_INTERVAL_DAYS: u32 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsrsMemory {
    pub stability: f32,
    pub difficulty: f32,
    pub last_reviewed_at: i64,
    pub history_reviews: u32,
}

impl FsrsMemory {
    pub(crate) fn is_valid(self, reviews: u32) -> bool {
        (0.001..=36_500.0).contains(&self.stability)
            && (1.0..=10.0).contains(&self.difficulty)
            && self.last_reviewed_at >= 0
            && (1..=reviews).contains(&self.history_reviews)
    }

    fn state(self) -> MemoryState {
        MemoryState {
            stability: self.stability,
            difficulty: self.difficulty,
        }
    }
}

pub(crate) const MAX_ELAPSED_MS: u32 = 86_400_000;

pub(crate) fn valid_answer(answer: &str) -> bool {
    !answer.trim().is_empty() && answer.len() <= 2_000
}

pub(crate) fn initialize(connection: &mut Connection) -> Result<()> {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    let (has_answer, has_elapsed, has_match) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('review_events') WHERE name='answer'),
                    EXISTS(SELECT 1 FROM pragma_table_info('review_events') WHERE name='elapsed_ms'),
                    EXISTS(SELECT 1 FROM pragma_table_info('review_events') WHERE name='matches_dictionary')",
            [],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)),
        )
        .map_err(storage_error)?;
    if !has_answer {
        tx.execute("ALTER TABLE review_events ADD COLUMN answer TEXT", [])
            .map_err(storage_error)?;
    }
    if !has_elapsed {
        tx.execute(
            "ALTER TABLE review_events ADD COLUMN elapsed_ms INTEGER",
            [],
        )
        .map_err(storage_error)?;
    }
    if !has_match {
        tx.execute(
            "ALTER TABLE review_events ADD COLUMN matches_dictionary INTEGER CHECK(matches_dictionary IN (0,1))",
            [],
        )
        .map_err(storage_error)?;
    }
    tx.commit().map_err(storage_error)
}

impl Store {
    pub fn vocabulary_review_history(&self, card_id: &str) -> Result<Vec<ReviewEvent>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT direction, grade, reviewed_at, answer, elapsed_ms, matches_dictionary FROM review_events
             WHERE entry_id=?1 ORDER BY reviewed_at DESC, id DESC LIMIT 20",
            )
            .map_err(storage_error)?;
        let events = statement
            .query_map([card_id], |row| {
                Ok(ReviewEvent {
                    entry_id: card_id.to_owned(),
                    direction: row.get(0)?,
                    grade: row.get(1)?,
                    reviewed_at: row.get(2)?,
                    answer: row.get(3)?,
                    elapsed_ms: row.get(4)?,
                    matches_dictionary: row.get(5)?,
                })
            })
            .map_err(storage_error)?;
        events.map(|event| event.map_err(storage_error)).collect()
    }
}

pub(crate) fn review(
    database: &Connection,
    card_id: &str,
    direction: &str,
    previous: &mut ReviewState,
    grade: u8,
    reviewed_at: i64,
) -> Result<()> {
    let model = FSRS::default();
    let memory = match previous.fsrs {
        Some(memory) => Some(memory),
        None => reconstruct_memory(database, &model, card_id, direction, previous)?,
    };
    let next = model
        .next_states(
            memory.map(FsrsMemory::state),
            RETENTION,
            memory.map_or(0, |memory| {
                elapsed_days(memory.last_reviewed_at, reviewed_at)
            }),
        )
        .map_err(model_error)?;
    let rating = match grade {
        0 => next.again,
        1 => next.hard,
        2 => next.good,
        3 => next.easy,
        _ => return Err("Некоректна відповідь".into()),
    };
    let history_reviews = memory.map_or(0, |memory| memory.history_reviews);
    let reviews = previous
        .reviews
        .max(history_reviews)
        .checked_add(1)
        .ok_or("Надто багато повторень картки.")?;
    let interval_days = if grade == 0 {
        0
    } else {
        (rating.interval.round() as u32).clamp(1, MAX_INTERVAL_DAYS)
    };
    previous.reviews = reviews;
    previous.interval_days = interval_days;
    previous.due_at = reviewed_at
        + if grade == 0 {
            AGAIN_SECONDS
        } else {
            i64::from(interval_days) * SECONDS_PER_DAY
        };
    previous.fsrs = Some(FsrsMemory {
        stability: rating.memory.stability,
        difficulty: rating.memory.difficulty,
        last_reviewed_at: reviewed_at,
        history_reviews: history_reviews + 1,
    });
    Ok(())
}

fn reconstruct_memory(
    database: &Connection,
    model: &FSRS,
    card_id: &str,
    direction: &str,
    previous: &ReviewState,
) -> Result<Option<FsrsMemory>> {
    let mut statement = database
        .prepare(
            "SELECT grade,reviewed_at FROM review_events
             WHERE entry_id=?1 AND direction=?2 ORDER BY reviewed_at,id",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![card_id, direction], |row| {
            Ok((row.get::<_, u8>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(storage_error)?;
    let mut last = None;
    let mut reviews = Vec::new();
    for row in rows {
        let (grade, reviewed_at) = row.map_err(storage_error)?;
        reviews.push(FSRSReview {
            rating: u32::from(grade) + 1,
            delta_t: last.map_or(0, |prior| elapsed_days(prior, reviewed_at)),
        });
        last = Some(reviewed_at);
    }
    let history_reviews = u32::try_from(reviews.len()).map_err(storage_error)?;
    let last_reviewed_at = last.unwrap_or_else(|| {
        let interval = if previous.interval_days == 0 {
            AGAIN_SECONDS
        } else {
            i64::from(previous.interval_days) * SECONDS_PER_DAY
        };
        previous.due_at.saturating_sub(interval).max(0)
    });
    let memory = if history_reviews > 0 && history_reviews >= previous.reviews {
        model
            .memory_state(FSRSItem { reviews }, None)
            .map_err(model_error)?
    } else if previous.reviews > 0 {
        // Old exports may have counts and due dates only. At 90% retention,
        // the old scheduled interval is our closest estimate of stability.
        MemoryState {
            stability: (previous.interval_days as f32).clamp(1.0, MAX_INTERVAL_DAYS as f32),
            difficulty: 5.0,
        }
    } else {
        return Ok(None);
    };
    Ok(Some(FsrsMemory {
        stability: memory.stability,
        difficulty: memory.difficulty,
        last_reviewed_at,
        history_reviews,
    }))
}

fn elapsed_days(earlier: i64, later: i64) -> u32 {
    let days = later.saturating_sub(earlier).max(0) / SECONDS_PER_DAY;
    u32::try_from(days).unwrap_or(u32::MAX)
}

fn model_error(error: fsrs::FSRSError) -> String {
    format!("Помилка планування повторення. {error}")
}

fn storage_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vocabulary;

    #[test]
    fn fsrs_intervals_respond_to_both_rating_and_elapsed_time() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("reader.db")).unwrap();
        let start = 20 * SECONDS_PER_DAY;
        let grade = |prior: ReviewState, grade, time| {
            let mut state = prior;
            review(
                &store.connection,
                "card",
                "forward",
                &mut state,
                grade,
                time,
            )
            .unwrap();
            state
        };
        let again = grade(ReviewState::default(), 0, start);
        let hard = grade(ReviewState::default(), 1, start);
        let good = grade(ReviewState::default(), 2, start);
        let easy = grade(ReviewState::default(), 3, start);
        assert_eq!(again.due_at, start + AGAIN_SECONDS);
        assert_eq!(hard.interval_days, 1);
        assert_eq!(good.interval_days, 2);
        assert_eq!(easy.interval_days, 8);
        assert_ne!(good.fsrs, easy.fsrs);

        let same_day = grade(good.clone(), 2, start + AGAIN_SECONDS);
        let eight_days = grade(good, 2, start + 8 * SECONDS_PER_DAY);
        assert!(same_day.interval_days < eight_days.interval_days);
        assert_eq!(same_day.reviews, 2);
        assert_eq!(eight_days.fsrs.unwrap().history_reviews, 2);
    }

    #[test]
    fn fsrs_keeps_legacy_due_dates_and_replays_each_direction() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let card = store
            .save_word(serde_json::from_str::<Vocabulary>(r#"{
                "id":"","word":"bonjour","translation":"привіт","language":"fr","targetLanguage":"uk",
                "context":"","documentId":"","forward":{"dueAt":0,"intervalDays":0,"reviews":0},
                "reverse":{"dueAt":0,"intervalDays":0,"reviews":0}
            }"#).unwrap())
            .unwrap();
        let mut legacy = card;
        let time = crate::now();
        legacy.forward = ReviewState {
            due_at: time,
            interval_days: 7,
            reviews: 2,
            fsrs: None,
        };
        let last = time - 7 * SECONDS_PER_DAY;
        store
            .connection
            .execute(
                "UPDATE vocabulary SET body=?2 WHERE id=?1",
                params![legacy.id, serde_json::to_string(&legacy).unwrap()],
            )
            .unwrap();
        for (grade, at) in [(2, last - SECONDS_PER_DAY), (3, last)] {
            store.connection.execute(
                "INSERT INTO review_events(entry_id,direction,grade,reviewed_at) VALUES (?1,'forward',?2,?3)",
                params![legacy.id, grade, at],
            ).unwrap();
        }

        let mut restored = Store::open(&root.path().join("restored.db")).unwrap();
        restored.restore(store.export().unwrap()).unwrap();
        let before = restored.dictionary_collection(None).unwrap().pop().unwrap();
        assert_eq!(before.forward.due_at, time);
        assert!(before.forward.fsrs.is_none());
        restored.review(&legacy.id, "forward", 2).unwrap();
        restored.review(&legacy.id, "reverse", 1).unwrap();
        let after = restored.dictionary_collection(None).unwrap().pop().unwrap();
        assert_eq!(after.forward.reviews, 3);
        assert_eq!(after.reverse.reviews, 1);
        assert_eq!(after.forward.fsrs.unwrap().history_reviews, 3);
        assert_eq!(after.reverse.interval_days, 1);
        assert_eq!(after.reverse.fsrs.unwrap().history_reviews, 1);

        let mut portable = Store::open(&root.path().join("portable.db")).unwrap();
        for _ in 0..2 {
            portable
                .import_dictionary(restored.export_dictionary(None).unwrap())
                .unwrap();
        }
        let imported = portable.dictionary_collection(None).unwrap().pop().unwrap();
        assert_eq!(imported.forward.fsrs, after.forward.fsrs);
        assert_eq!(imported.reverse.fsrs, after.reverse.fsrs);

        portable.review(&legacy.id, "reverse", 2).unwrap();
        restored.review(&legacy.id, "reverse", 3).unwrap();
        restored
            .import_dictionary(portable.export_dictionary(None).unwrap())
            .unwrap();
        let merged = restored.dictionary_collection(None).unwrap().pop().unwrap();
        assert_eq!(merged.forward.fsrs, after.forward.fsrs);
        assert_eq!(merged.reverse.reviews, 3);
        assert!(merged.reverse.fsrs.is_none());
        restored.review(&legacy.id, "reverse", 2).unwrap();
        let merged = restored.dictionary_collection(None).unwrap().pop().unwrap();
        assert_eq!(merged.reverse.fsrs.unwrap().history_reviews, 4);

        let recent = restored.vocabulary_review_history(&legacy.id).unwrap();
        assert_eq!(recent.len(), 7);
        assert_eq!(
            recent
                .iter()
                .filter(|event| event.direction == "reverse")
                .count(),
            4
        );
        assert!(recent
            .windows(2)
            .all(|events| events[0].reviewed_at >= events[1].reviewed_at));
    }
}
