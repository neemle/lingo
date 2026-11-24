using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
#if MACCATALYST
    private CancellationTokenSource? pdfPreparation;
#endif

    private async Task<string?> CompleteStagedIntakeAsync(IntakeItem item, ShareInboxItem? source = null,
        bool chooseLanguage = false)
    {
        if (item.DocumentId is { } completed) return completed;
        var session = AppState.Session;
        string? selectedLanguage = null;
        if (chooseLanguage)
        {
            selectedLanguage = source is not null
                ? await ResolveImportLanguageAsync(source)
                : await ChooseLanguageAsync("Мова оригіналу", item.Name);
            if (selectedLanguage is null) return null;
        }

        Task<string?> ChooseAsync(Func<Task<string?>> detect) =>
            selectedLanguage is not null ? Task.FromResult<string?>(selectedLanguage) :
            source is not null ? ResolveImportLanguageAsync(source, detect) :
            DetectOrChooseStagedLanguageAsync(item.Name, detect);

        switch (item.NextAction)
        {
            case "importDocument":
            {
                var language = await ChooseAsync(async () =>
                    await ImportLanguageDetection.DetectDocumentAsync(session, item.Name, await session.ReadIntakeBytesAsync(item.Id)));
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
                    bytes = await session.ReadIntakeBytesAsync(item.Id);
                    ImportStatus.Text = "Визначаємо мову зображення…";
                    var sample = await TesseractOcr.ReadLanguageSampleAsync(() => PhotoOcr.PreviewAsync(bytes));
                    if (sample.Length > 0 && await ImportLanguageDetection.DetectAsync(session, sample) is { } detected)
                        return detected;
#if MACCATALYST
                    return await ImportLanguageDetection.DetectAsync(session, await PhotoOcr.ReadLanguageSampleAsync(bytes));
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
                var language = await ChooseAsync(() => ImportLanguageDetection.DetectAsync(session, article.Text));
                if (language is null) return null;
                await session.WaitUntilIdleAsync();
                await session.CompleteStagedArticleIntakeAsync(item.Id, language);
                break;
            }
#if MACCATALYST
            case "readPdfPages":
                if (!await PrepareStagedPdfAsync(item, ChooseAsync)) return null;
                break;
#endif
            default:
                throw new InvalidDataException("Цей тип вхідного матеріалу не підтримується на пристрої.");
        }
        if (session.Error.Length > 0) throw new InvalidDataException(session.Error);
        return session.Document?.Id;
    }

#if MACCATALYST
    private async Task<bool> PrepareStagedPdfAsync(IntakeItem item, Func<Func<Task<string?>>, Task<string?>> choose)
    {
        using var preparation = new CancellationTokenSource();
        var token = preparation.Token;
        pdfPreparation = preparation;
        StopImportStep.Text = "Призупинити підготовку PDF";
        StopImportStep.IsVisible = StopImportStep.IsEnabled = true;
        var session = AppState.Session;
        try
        {
            string? path = null;
            if (await session.IntakePdfTextSampleAsync(item.Id) is null)
            {
                path = (await session.IntakeFileAsync(item.Id)).Path;
                token.ThrowIfCancellationRequested();
                ImportStatus.Text = "Читаємо текстовий шар PDF…";
                var (pageTexts, deferredFrom) = await PdfScan.ReadTextForIntakeAsync(path, token);
                token.ThrowIfCancellationRequested();
                await session.SaveIntakePdfTextAsync(item.Id, pageTexts, deferredFrom);
            }
            token.ThrowIfCancellationRequested();
            var language = await choose(async () =>
            {
                token.ThrowIfCancellationRequested();
                ImportStatus.Text = "Визначаємо мову за кількома сторінками PDF…";
                path ??= (await session.IntakeFileAsync(item.Id)).Path;
                var samples = await PdfScan.ReadLanguageSamplesAsync(path, PdfScan.LanguageSample.TextLayer, cancellation: token);
                Task<string?> DetectAsync(string sample) => ImportLanguageDetection.DetectAsync(session, sample);
                return await PdfRecognition.DetectSampleLanguageAsync(samples, DetectAsync, token,
                    (slots, stop) => PdfScan.ReadLanguageSamplesAsync(path, PdfScan.LanguageSample.PortableOcr,
                        slots, stop, token),
                    (slots, stop) => PdfScan.ReadLanguageSamplesAsync(path, PdfScan.LanguageSample.NativeOcr,
                        slots, stop, token));
            });
            if (language is null) return false;
            await session.WaitUntilIdleAsync();
            token.ThrowIfCancellationRequested();
            StopImportStep.IsVisible = false;
            ImportStatus.Text = "Зберігаємо документ…";
            await session.CompleteStagedPdfIntakeAsync(item.Id, language);
            return true;
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested)
        {
            return false;
        }
        finally
        {
            pdfPreparation = null;
            StopImportStep.IsVisible = false;
        }
    }
#endif

    private async Task<string?> DetectOrChooseStagedLanguageAsync(string name, Func<Task<string?>> detect) =>
        SupportedLanguage(await detect()) ?? await ChooseLanguageAsync("Мова оригіналу", name);
}
