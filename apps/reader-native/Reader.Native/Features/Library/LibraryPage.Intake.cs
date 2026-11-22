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
                if (await session.IntakePhotoOcrLanguageAsync(item.Id) != language)
                {
                    var bytes = await session.ReadIntakeBytesAsync(item.Id);
                    ImportStatus.Text = "Розпізнавання тексту на пристрої…";
                    var lines = await PhotoOcr.RecognizeAsync(bytes, language);
                    if (lines.Length == 0) throw new InvalidDataException("На зображенні не знайдено тексту.");
                    await session.SaveIntakePhotoOcrAsync(item.Id, language, lines);
                }
                await session.WaitUntilIdleAsync();
                await session.CompleteStagedPhotoIntakeAsync(item.Id, language);
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
                var excerpt = await session.IntakePdfTextSampleAsync(item.Id);
                if (excerpt is null)
                {
                    var original = await session.IntakeFileAsync(item.Id);
                    ImportStatus.Text = "Читаємо текстовий шар PDF…";
                    var (pageTexts, deferredFrom) = await PdfScan.ReadTextForIntakeAsync(original.Path);
                    excerpt = await session.SaveIntakePdfTextAsync(item.Id, pageTexts, deferredFrom);
                }
                var language = await ChooseAsync(() => source?.ScanPdf == true || excerpt.Length == 0
                    ? Task.FromResult<string?>(null) : session.DetectLanguageAsync(excerpt));
                if (language is null) return null;
                ImportStatus.Text = "Зберігаємо документ…";
                await session.WaitUntilIdleAsync();
                await session.CompleteStagedPdfIntakeAsync(item.Id, language);
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
