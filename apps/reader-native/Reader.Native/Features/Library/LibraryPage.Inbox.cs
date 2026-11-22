using System.Text;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private async Task RefreshShareInboxAsync()
    {
        try
        {
            var items = await Task.Run(ShareInbox.List);
            OpenShareInbox.Text = $"Вхідні матеріали ({items.Length})";
            OpenShareInbox.IsVisible = items.Length > 0;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            ImportStatus.Text = $"Не вдалося прочитати вхідні матеріали. {error.Message}";
        }
    }

    private async void OpenShareInboxClicked(object? sender, EventArgs args)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        try
        {
            var items = await Task.Run(ShareInbox.List);
            if (items.Length == 0) return;
            var choices = items.Select((item, index) => $"{index + 1}. {item.Label}").ToArray();
            var choice = await DisplayActionSheetAsync("Вхідні матеріали", "Закрити", null, choices);
            var selected = Array.IndexOf(choices, choice);
            if (selected < 0) return;
            var item = items[selected];
            string[] actions = item.Kind == "damaged" ? [] : item.PhotoLanguage is null
                ? ["Продовжити імпорт"] : ["Продовжити імпорт", "Змінити мову OCR"];
            var action = await DisplayActionSheetAsync(item.Name, "Скасувати", "Видалити копію", actions);
            if (action == "Видалити копію" && await DisplayAlertAsync("Видалити вхідний матеріал?", item.Name, "Видалити", "Скасувати"))
                await RemoveShareItemAsync(item);
            if (action is "Продовжити імпорт" or "Змінити мову OCR")
                await ImportExternalAsync(item, choosePhotoLanguage: action == "Змінити мову OCR");
        }
        catch (Exception error) { await DisplayAlertAsync("Вхідні матеріали", error.Message, "Гаразд"); }
        finally
        {
            DesktopImport.Gate.Release();
            await RefreshShareInboxAsync();
        }
    }

    internal async Task ImportExternalAsync(ShareInboxItem item, bool choosePhotoLanguage = false)
    {
        await loading;
        if (!ShareInbox.Contains(item)) return;
        importing = true;
        try
        {
            var bytes = await ShareInbox.ReadAsync(item);
            var id = item.Kind switch
            {
                "file" => await ImportFileAsync(new DesktopFile(item.Name, () => Task.FromResult(bytes)),
                    scanPdf: false, staged: item, choosePhotoLanguage: choosePhotoLanguage),
                "text" => await ImportPastedTextCoreAsync(Encoding.UTF8.GetString(bytes)),
                "url" => await ImportArticleCoreAsync(WebArticles.Address(Encoding.UTF8.GetString(bytes))
                    ?? throw new InvalidDataException("Збережене вебпосилання недійсне.")),
                _ => throw new InvalidDataException("Вхідний матеріал пошкоджено."),
            };
            if (id is null) return;
            await RemoveShareItemAsync(item);
            await AppState.OpenAsync(id);
        }
        catch (Exception error) { await DisplayAlertAsync(item.Name, error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
            await RefreshShareInboxAsync();
        }
    }

    private Task RemoveShareItemAsync(ShareInboxItem item) => Task.Run(() => ShareInbox.Remove(item));
}
