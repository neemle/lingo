using System.Text;
using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private async void ImportArticleClicked(object? sender, EventArgs args) => await ImportArticleAsync(null);

    private async Task ImportArticleAsync(Uri? suggested)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        importing = true;
        try
        {
            var input = await DisplayPromptAsync("Додати статтю", "Вставте адресу сторінки. Читай завантажить її текст і збереже на пристрої.",
                "Завантажити", "Скасувати", "https://…", 4096, Keyboard.Url, suggested?.AbsoluteUri ?? "");
            if (input is null) return;
            var address = WebArticles.Address(input) ?? throw new InvalidDataException("Введіть дійсну адресу HTTP або HTTPS.");
            ImportStatus.Text = "Завантажуємо статтю…";
            var article = await Task.Run(() => WebArticles.DownloadAsync(address));
            var session = AppState.Session;
            await session.WaitUntilIdleAsync();
            ImportStatus.Text = "";
            var language = await ChooseLanguageAsync("Мова оригіналу", await session.DetectLanguageAsync(article.Text));
            if (language is null) return;
            await session.ImportAsync(article.Title + ".txt", Encoding.UTF8.GetBytes(article.Text), language,
                article.Address.AbsoluteUri);
            if (session.Error.Length == 0 && session.Document is { } document)
                await AppState.OpenAsync(document.Id);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт статті", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
            DesktopImport.Gate.Release();
        }
    }

    private async void OpenArticleSourceClicked(object? sender, EventArgs args)
    {
        if (sender is not Button { CommandParameter: Document document } || WebArticles.Address(document.SourceUrl) is not { } address)
            return;
        try { await Launcher.Default.OpenAsync(address); }
        catch (Exception error) { await DisplayAlertAsync("Джерело статті", error.Message, "Гаразд"); }
    }
}
