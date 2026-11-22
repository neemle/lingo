using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private async Task<string?> CompleteStagedIntakeAsync(IntakeItem item, ShareInboxItem? source = null,
        bool chooseLanguage = false)
    {
        if (item.DocumentId is { } completed) return completed;
        var session = AppState.Session;

        Task<string?> ChooseAsync(Func<Task<string?>> detect) => source is not null
            ? ResolveImportLanguageAsync(source, chooseLanguage, detect)
            : ChooseStagedLanguageAsync(item.Name, detect);

        switch (item.NextAction)
        {
            case "importDocument":
            {
                var language = await ChooseAsync(async () =>
                    await session.DetectDocumentLanguageAsync(item.Name, await session.ReadIntakeBytesAsync(item.Id)));
                if (language is null) return null;
                await session.WaitUntilIdleAsync();
                await session.CompleteDocumentIntakeAsync(item.Id, language);
                break;
            }
            case "recognizePhoto":
            {
                var language = await ChooseAsync(() => Task.FromResult<string?>(null));
                if (language is null) return null;
                var bytes = await session.ReadIntakeBytesAsync(item.Id);
                ImportStatus.Text = "Розпізнавання тексту на пристрої…";
                var lines = await PhotoOcr.RecognizeAsync(bytes, language);
                if (lines.Length == 0) throw new InvalidDataException("На зображенні не знайдено тексту.");
                await session.WaitUntilIdleAsync();
                await session.CompletePhotoIntakeAsync(item.Id, language, lines);
                break;
            }
            case "extractArticle":
            {
                var article = await session.IntakeArticleAsync(item.Id);
                if (article is null)
                {
                    var url = await session.ReadIntakeLinkAsync(item.Id);
                    var address = WebArticles.Address(url) ?? throw new InvalidDataException("Збережене вебпосилання недійсне.");
                    ImportStatus.Text = "Завантажуємо статтю…";
                    var downloaded = await Task.Run(() => WebArticles.DownloadAsync(address));
                    article = await session.SaveIntakeArticleAsync(item.Id,
                        new IntakeArticleDraft(downloaded.Title, downloaded.Text, downloaded.Address.AbsoluteUri));
                }
                ImportStatus.Text = "";
                var language = await ChooseAsync(() => session.DetectLanguageAsync(article.Text));
                if (language is null) return null;
                await session.WaitUntilIdleAsync();
                await session.CompleteStagedArticleIntakeAsync(item.Id, language);
                break;
            }
#if MACCATALYST
            case "readPdfPages":
            {
                var original = await session.IntakeFileAsync(item.Id);
                ImportStatus.Text = "Читаємо текстовий шар PDF…";
                var (pageTexts, deferredFrom) = await PdfScan.ReadTextForIntakeAsync(original.Path);
                var language = await ChooseAsync(() =>
                {
                    if (source?.ScanPdf == true) return Task.FromResult<string?>(null);
                    var excerpt = string.Join('\n', pageTexts.Where(text => !string.IsNullOrWhiteSpace(text)).Take(4)
                        .Select(text => text[..Math.Min(text.Length, 2000)]));
                    return session.DetectLanguageAsync(excerpt);
                });
                if (language is null) return null;
                ImportStatus.Text = "Зберігаємо документ…";
                await session.WaitUntilIdleAsync();
                await session.CompletePdfIntakeAsync(item.Id, language, pageTexts, deferredFrom);
                break;
            }
#endif
            default:
                throw new InvalidDataException("Цей тип вхідного матеріалу не підтримується на пристрої.");
        }
        if (session.Error.Length > 0) throw new InvalidDataException(session.Error);
        return session.Document?.Id;
    }

    private async Task<string?> ChooseStagedLanguageAsync(string name, Func<Task<string?>> detect) =>
        await ChooseLanguageAsync($"Мова оригіналу · {name}", await detect());
}
