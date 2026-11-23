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
            : DetectOrChooseStagedLanguageAsync(item.Name, detect);

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
                var cachedLanguage = await session.IntakePhotoOcrLanguageAsync(item.Id);
                byte[]? bytes = null;
                var language = await ChooseAsync(async () =>
                {
                    if (cachedLanguage is not null) return cachedLanguage;
#if MACCATALYST
                    bytes = await session.ReadIntakeBytesAsync(item.Id);
                    ImportStatus.Text = "Визначаємо мову зображення…";
                    return await session.DetectLanguageAsync(await PhotoOcr.ReadLanguageSampleAsync(bytes));
#else
                    return null;
#endif
                });
                if (language is null) return null;
                if (cachedLanguage != language)
                {
                    bytes ??= await session.ReadIntakeBytesAsync(item.Id);
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
                string? path = null;
                if (await session.IntakePdfTextSampleAsync(item.Id) is null)
                {
                    path = (await session.IntakeFileAsync(item.Id)).Path;
                    ImportStatus.Text = "Читаємо текстовий шар PDF…";
                    var (pageTexts, deferredFrom) = await PdfScan.ReadTextForIntakeAsync(path);
                    await session.SaveIntakePdfTextAsync(item.Id, pageTexts, deferredFrom);
                }
                var language = await ChooseAsync(async () =>
                {
                    ImportStatus.Text = "Визначаємо мову за кількома сторінками PDF…";
                    path ??= (await session.IntakeFileAsync(item.Id)).Path;
                    var samples = await PdfScan.ReadLanguageSamplesAsync(path, optical: false);
                    var detected = await PdfRecognition.DetectSampleLanguageAsync(samples, session.DetectLanguageAsync);
                    if (detected is not null) return detected;

                    const int bodySamples = 3;
                    var body = await PdfScan.ReadLanguageSamplesAsync(path, optical: true, count: bodySamples);
                    detected = await PdfRecognition.DetectSampleLanguageAsync(body, session.DetectLanguageAsync);
                    if (detected is not null || samples.Length <= bodySamples) return detected;

                    var front = await PdfScan.ReadLanguageSamplesAsync(path, optical: true, skip: bodySamples);
                    return await PdfRecognition.DetectSampleLanguageAsync([.. body, .. front], session.DetectLanguageAsync);
                });
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

    private async Task<string?> DetectOrChooseStagedLanguageAsync(string name, Func<Task<string?>> detect) =>
        SupportedLanguage(await detect()) ?? await ChooseLanguageAsync("Мова оригіналу", name);
}
