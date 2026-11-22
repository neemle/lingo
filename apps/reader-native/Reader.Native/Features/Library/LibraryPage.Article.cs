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
            if (await ImportArticleCoreAsync(suggested) is { } id) await AppState.OpenAsync(id);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт статті", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
            DesktopImport.Gate.Release();
            await RefreshShareInboxAsync();
        }
    }

    private async Task<string?> ImportArticleCoreAsync(Uri? suggested, ShareInboxItem? source = null)
    {
        var input = await DisplayPromptAsync("Додати статтю", "Вставте адресу сторінки. Читай завантажить її текст і збереже на пристрої.",
            "Завантажити", "Скасувати", "https://…", 4096, Keyboard.Url, suggested?.AbsoluteUri ?? "");
        if (input is null) return null;
        var address = WebArticles.Address(input) ?? throw new InvalidDataException("Введіть дійсну адресу HTTP або HTTPS.");
        var session = AppState.Session;
        var intake = await session.StageLinkIntakeAsync(source?.Id ?? Guid.NewGuid(), address.AbsoluteUri);
        return await CompleteStagedIntakeAsync(intake, source);
    }

    private async void OpenArticleSourceClicked(object? sender, EventArgs args)
    {
        if (sender is not Button { CommandParameter: Document document } || WebArticles.Address(document.SourceUrl) is not { } address)
            return;
        try { await Launcher.Default.OpenAsync(address); }
        catch (Exception error) { await DisplayAlertAsync("Джерело статті", error.Message, "Гаразд"); }
    }
}
