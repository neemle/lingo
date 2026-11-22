use candle_core::Tensor;
use candle_transformers::models::marian::{Config, MTModel};

use crate::{model_storage::model_error, Result};

const MAX_BEAMS: usize = 8;
const MAX_TOKENS: usize = 256;

trait DecoderState: Clone {
    fn logits(&mut self, token: u32, position: usize) -> Result<Vec<f32>>;
}

#[derive(Clone)]
struct CandleState {
    model: MTModel,
    encoded: Tensor,
}

impl DecoderState for CandleState {
    fn logits(&mut self, token: u32, position: usize) -> Result<Vec<f32>> {
        let input = Tensor::new(&[[token]], self.encoded.device()).map_err(model_error)?;
        self.model
            .decode(&input, &self.encoded, position)
            .and_then(|output| output.get(0)?.get(0)?.to_vec1::<f32>())
            .map_err(model_error)
    }
}

#[derive(Clone, Copy)]
struct Options {
    start: u32,
    eos: u32,
    forced_eos: u32,
    pad: u32,
    width: usize,
    limit: usize,
}

impl Options {
    fn ends(self, token: u32) -> bool {
        token == self.eos || token == self.forced_eos
    }
}

struct Beam<S> {
    state: S,
    tokens: Vec<u32>,
    last: u32,
    log_probability: f64,
}

struct Candidate {
    parent: usize,
    token: u32,
    log_probability: f64,
}

struct Finished {
    tokens: Vec<u32>,
    score: f64,
}

pub(super) fn decode(
    model: &MTModel,
    encoded: &Tensor,
    config: &Config,
    width: u8,
    limit: usize,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<Vec<u32>> {
    candidates(model, encoded, config, width, limit, checkpoint)?
        .into_iter()
        .next()
        .ok_or_else(|| "Модель не завершила переклад речення".into())
}

pub(super) fn candidates(
    model: &MTModel,
    encoded: &Tensor,
    config: &Config,
    width: u8,
    limit: usize,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<Vec<Vec<u32>>> {
    search_candidates(
        CandleState {
            model: model.clone(),
            encoded: encoded.clone(),
        },
        Options {
            start: config.decoder_start_token_id,
            eos: config.eos_token_id,
            forced_eos: config.forced_eos_token_id,
            pad: config.pad_token_id,
            width: usize::from(width),
            limit,
        },
        checkpoint,
    )
    .map(|finished| finished.into_iter().map(|beam| beam.tokens).collect())
}

#[cfg(test)]
fn search<S: DecoderState>(initial: S, options: Options) -> Result<Vec<u32>> {
    search_candidates(initial, options, &|| Ok(()))?
        .into_iter()
        .next()
        .map(|beam| beam.tokens)
        .ok_or_else(|| "Модель не завершила переклад речення".into())
}

fn search_candidates<S: DecoderState>(
    mut initial: S,
    options: Options,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<Vec<Finished>> {
    if !(1..=MAX_BEAMS).contains(&options.width) || !(1..=MAX_TOKENS).contains(&options.limit) {
        return Err("Некоректні межі декодування перекладу".into());
    }
    if options.width == 1 {
        let mut last = options.start;
        let mut output = Vec::new();
        for position in 0..options.limit {
            checkpoint()?;
            last = initial
                .logits(last, position)?
                .into_iter()
                .enumerate()
                .filter(|(id, score)| *id != options.pad as usize && score.is_finite())
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(id, _)| id as u32)
                .ok_or("Модель не повернула доступного токена")?;
            if options.ends(last) {
                return Ok(vec![Finished {
                    tokens: output,
                    score: 0.0,
                }]);
            }
            output.push(last);
        }
        return Err("Модель не завершила переклад речення".into());
    }

    let mut active = vec![Beam {
        state: initial,
        tokens: Vec::new(),
        last: options.start,
        log_probability: 0.0,
    }];
    let mut finished: Vec<Finished> = Vec::new();
    for position in 0..options.limit {
        let mut candidates = Vec::with_capacity(2 * options.width * options.width);
        for (parent, beam) in active.iter_mut().enumerate() {
            checkpoint()?;
            let logits = beam.state.logits(beam.last, position)?;
            for (token, score) in best_tokens(&logits, options.pad, 2 * options.width)? {
                candidates.push(Candidate {
                    parent,
                    token,
                    log_probability: beam.log_probability + score,
                });
            }
        }
        candidates.sort_by(|a, b| {
            b.log_probability
                .total_cmp(&a.log_probability)
                .then_with(|| active[a.parent].tokens.cmp(&active[b.parent].tokens))
                .then_with(|| a.token.cmp(&b.token))
        });
        let mut next = Vec::with_capacity(options.width);
        for (rank, candidate) in candidates.into_iter().enumerate() {
            let parent = &active[candidate.parent];
            if options.ends(candidate.token) {
                if rank < options.width {
                    finished.push(Finished {
                        tokens: parent.tokens.clone(),
                        score: candidate.log_probability / (position + 1) as f64,
                    });
                }
            } else if next.len() < options.width {
                let mut tokens = parent.tokens.clone();
                tokens.push(candidate.token);
                // MTModel's cloned tensors share immutable weights and the current KV prefix;
                // Candle appends a new cache tensor when this child advances.
                next.push(Beam {
                    state: parent.state.clone(),
                    tokens,
                    last: candidate.token,
                    log_probability: candidate.log_probability,
                });
            }
            if next.len() == options.width && rank + 1 >= options.width {
                break;
            }
        }
        finished.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.tokens.cmp(&b.tokens))
        });
        finished.truncate(options.width);
        if next.is_empty() {
            break;
        }
        // Standard current-length beam early stopping. Completed hypotheses remain bounded
        // and a later unfinished path cannot replace an EOS-completed result at the cap.
        if finished.len() == options.width {
            let best_active = next
                .iter()
                .map(|beam| beam.log_probability / (position + 1) as f64)
                .fold(f64::NEG_INFINITY, f64::max);
            if finished
                .last()
                .is_some_and(|last| last.score >= best_active)
            {
                break;
            }
        }
        active = next;
    }
    if finished.is_empty() {
        Err("Модель не завершила переклад речення".into())
    } else {
        Ok(finished)
    }
}

fn best_tokens(logits: &[f32], pad: u32, count: usize) -> Result<Vec<(u32, f64)>> {
    let mut values: Vec<_> = logits
        .iter()
        .copied()
        .enumerate()
        .filter(|(id, value)| *id != pad as usize && value.is_finite())
        .map(|(id, value)| (id as u32, f64::from(value)))
        .collect();
    let maximum = values
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::NEG_INFINITY, f64::max);
    if !maximum.is_finite() {
        return Err("Модель не повернула доступного токена".into());
    }
    let normalizer = maximum
        + values
            .iter()
            .map(|(_, value)| (*value - maximum).exp())
            .sum::<f64>()
            .ln();
    let order = |a: &(u32, f64), b: &(u32, f64)| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    if values.len() > count {
        values.select_nth_unstable_by(count, order);
        values.truncate(count);
    }
    values.sort_by(order);
    for (_, value) in &mut values {
        *value -= normalizer;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Branching {
        prefix: Vec<u32>,
    }
    impl DecoderState for Branching {
        fn logits(&mut self, token: u32, _position: usize) -> Result<Vec<f32>> {
            self.prefix.push(token);
            let scores = match self.prefix.as_slice() {
                [3] => vec![-20.0, 0.55f32.ln(), 0.45f32.ln(), f32::INFINITY],
                [3, 1] => vec![0.51f32.ln(), 0.49f32.ln(), -20.0, f32::INFINITY],
                [3, 2] => vec![0.99f32.ln(), 0.01f32.ln(), -20.0, f32::INFINITY],
                _ => vec![0.0, -20.0, -20.0, f32::INFINITY],
            };
            Ok(scores)
        }
    }
    fn options(width: usize) -> Options {
        Options {
            start: 3,
            eos: 0,
            forced_eos: 0,
            pad: 3,
            width,
            limit: 8,
        }
    }
    #[test]
    fn beam_recovers_a_better_completed_branch_without_crossing_kv_prefixes() {
        let state = Branching { prefix: Vec::new() };
        assert_eq!(search(state.clone(), options(1)).unwrap(), vec![1]);
        assert_eq!(search(state, options(2)).unwrap(), vec![2]);
    }
    #[test]
    fn top_tokens_mask_padding_invalid_scores_and_have_stable_ties() {
        let output = best_tokens(&[f32::NAN, 1.0, 1.0, f32::INFINITY, 0.0], 3, 2).unwrap();
        assert_eq!(
            output.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(output
            .iter()
            .all(|(_, score)| score.is_finite() && *score <= 0.0));
        assert!(best_tokens(&[f32::NAN, f32::NEG_INFINITY], 9, 2).is_err());
    }
    #[test]
    fn decoder_rejects_unbounded_settings_and_unterminated_outputs() {
        let state = Branching { prefix: Vec::new() };
        assert!(search(state.clone(), options(0)).is_err());
        assert!(search(state.clone(), options(9)).is_err());
        let mut short = options(1);
        short.limit = 1;
        assert!(search(state.clone(), short).is_err());
        let mut large = options(1);
        large.limit = 257;
        assert!(search(state, large).is_err());
    }
}
