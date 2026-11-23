using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

internal sealed class OcrPageReviewFlow
{
    internal sealed record Source(string DocumentId, int Page, bool Detached, string Revision);
    internal sealed record Applied(DetachedPdfOcrPage? DetachedPage);

    private Source? previousSource;
    private readonly HashSet<int> reviewedIndices = [];
    private int remainingLines;
    internal bool IsRunning { get; private set; }

    internal string ButtonText(Source? source) => remainingLines > 0 && previousSource == source
        ? "Звірити наступні рядки" : "Звірити сторінку із Qwen";

    internal async Task<Applied?> RunAsync(ReaderSession session, INavigation navigation, Source source,
        Func<bool> isReadingPage, Action<string> status)
    {
        if (IsRunning) return null;
        IsRunning = true;
        try { return await ReviewAsync(session, navigation, source, isReadingPage, status); }
        finally { IsRunning = false; }
    }

    private async Task<Applied?> ReviewAsync(ReaderSession session, INavigation navigation, Source source,
        Func<bool> isReadingPage, Action<string> status)
    {
        bool IsCurrent() => isReadingPage() && session.Document is { } document &&
            document.Id == source.DocumentId && (source.Detached || document.CurrentRevisionId == source.Revision);

        if (!IsCurrent()) return null;
        if (previousSource != source || remainingLines == 0)
        {
            reviewedIndices.Clear();
            remainingLines = 0;
        }
        var reviewed = reviewedIndices.ToArray();
        var review = await LoadAsync(session, IsCurrent, status, "Qwen перевіряє рядки з низькою впевненістю OCR…",
            cachedOnly => session.ReviewOcrPageAsync(source.DocumentId, source.Page, source.Detached,
                source.Revision, cachedOnly, reviewed));
        if (review is null || !IsCurrent()) return null;
        if (review.Corrections.Length == 0)
        {
            Finish(review, source, review.CheckedLines == 0 ? "Немає рядків із низькою впевненістю OCR."
                : $"Перевірено рядків: {review.CheckedLines}. Виправлень немає.", status);
            return null;
        }

        var selected = await OcrCorrectionReviewPage.ChooseAsync(navigation, review.Corrections);
        if (selected is null || !IsCurrent() || session.Busy) return null;
        if (selected.Length == 0)
        {
            Finish(review, source, "Рядки залишено як є.", status);
            return null;
        }

        await session.ApplyOcrPageReviewAsync(source.DocumentId, source.Page, source.Detached, source.Revision, selected);
        if (!isReadingPage() || session.Document?.Id != source.DocumentId) return null;
        if (session.Error.Length > 0)
        {
            status(session.Error);
            return null;
        }

        var updatedPage = source.Detached ? await session.DetachedPdfOcrAsync(source.DocumentId, source.Page) : null;
        if (!isReadingPage() || session.Document?.Id != source.DocumentId || (source.Detached && updatedPage is null))
            return null;
        var updatedRevision = updatedPage?.Revision ?? session.Document?.CurrentRevisionId;
        if (updatedRevision is not null)
            Finish(review, source with { Revision = updatedRevision }, $"Виправлено рядків: {selected.Length}.", status);
        return new Applied(updatedPage);
    }

    internal static async Task<T?> LoadAsync<T>(ReaderSession session, Func<bool> isCurrent, Action<string> status,
        string loadingMessage, Func<bool, Task<T?>> review) where T : class
    {
        status("Шукаємо збережену перевірку…");
        var hint = await review(true);
        if (hint is not null || !isCurrent()) return hint;
        var model = (await session.OptionalModelsAsync()).FirstOrDefault(item => item.CanGenerateLanguageHints);
        if (!isCurrent()) return null;
        if (model is not { FilesPresent: true })
        {
            status("Установіть Qwen 2B у налаштуваннях.");
            return null;
        }
        status(loadingMessage);
        return await review(false);
    }

    private void Finish(OcrPageReview review, Source source, string message, Action<string> status)
    {
        reviewedIndices.UnionWith(review.ReviewedIndices);
        previousSource = source;
        remainingLines = review.RemainingLines;
        status(remainingLines == 0 ? message : $"{message} Ще рядків для перевірки: {remainingLines}.");
    }
}
