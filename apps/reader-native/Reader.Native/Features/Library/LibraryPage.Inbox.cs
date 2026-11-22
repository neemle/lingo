using System.Text;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private bool pauseExternalBatch;

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
            const string resumeAll = "Продовжити імпорт усіх";
            var recoverable = items.Where(item => item.Kind != "damaged").ToArray();
            var itemChoices = items.Select((item, index) => $"{index + 1}. {item.Label}").ToArray();
            string[] choices = recoverable.Length > 1 ? [resumeAll, .. itemChoices] : itemChoices;
            var choice = await DisplayActionSheetAsync("Вхідні матеріали", "Закрити", null, choices);
            if (choice == resumeAll)
            {
                await ImportExternalBatchAsync(recoverable);
                return;
            }
            var selected = Array.IndexOf(itemChoices, choice);
            if (selected < 0) return;
            var item = items[selected];
            const string changeLanguage = "Змінити мову оригіналу";
            string[] actions = item.Kind == "damaged" ? [] : item.SourceLanguage is null
                ? ["Продовжити імпорт"] : ["Продовжити імпорт", changeLanguage];
            var action = await DisplayActionSheetAsync(item.Name, "Скасувати", "Видалити копію", actions);
            if (action == "Видалити копію" && await DisplayAlertAsync("Видалити вхідний матеріал?", item.Name, "Видалити", "Скасувати"))
                await RemoveShareItemAsync(item);
            if (action is "Продовжити імпорт" or changeLanguage)
                await ImportExternalAsync(item, chooseLanguage: action == changeLanguage);
        }
        catch (Exception error) { await DisplayAlertAsync("Вхідні матеріали", error.Message, "Гаразд"); }
        finally
        {
            DesktopImport.Gate.Release();
            await RefreshShareInboxAsync();
        }
    }

    private Task ImportExternalAsync(ShareInboxItem item, bool chooseLanguage = false) =>
        ImportExternalBatchAsync([item], chooseLanguage);

    internal async Task ImportExternalBatchAsync(ShareInboxItem[] items, bool chooseLanguage = false)
    {
        await loading;
        importing = true;
        pauseExternalBatch = false;
        PauseExternalBatch.IsEnabled = true;
        try
        {
            string? lastDocument = null;
            for (var index = 0; index < items.Length && !pauseExternalBatch; index++)
            {
                PauseExternalBatch.IsVisible = index + 1 < items.Length;
                var item = items[index];
                try { lastDocument = await ImportInboxItemAsync(item, chooseLanguage) ?? lastDocument; }
                catch (Exception error) { await DisplayAlertAsync(item.Name, error.Message, "Гаразд"); }
            }
            if (!pauseExternalBatch && lastDocument is not null) await AppState.OpenAsync(lastDocument);
        }
        finally
        {
            PauseExternalBatch.IsVisible = false;
            importing = false;
            ImportStatus.Text = pauseExternalBatch
                ? "Імпорт призупинено. Решта добірки доступна у вхідних матеріалах." : "";
            await RefreshShareInboxAsync();
        }
    }

    private void PauseExternalBatchClicked(object? sender, EventArgs args)
    {
        if (!importing || !PauseExternalBatch.IsVisible) return;
        pauseExternalBatch = true;
        PauseExternalBatch.IsEnabled = false;
        ImportStatus.Text = "Імпорт призупиниться після поточного матеріалу.";
    }

    private async Task<string?> ImportInboxItemAsync(ShareInboxItem item, bool chooseLanguage = false)
    {
        if (!ShareInbox.Contains(item)) return null;
        string? id;
#if MACCATALYST
        if (item.Kind == "file" && item.Bytes > Reader.CoreClient.CoreClient.MaxImportBytes
            && Path.GetExtension(item.Name).Equals(".pdf", StringComparison.OrdinalIgnoreCase))
            id = await ImportLargePdfAsync(item, chooseLanguage);
        else
#endif
        {
            var bytes = await ShareInbox.ReadAsync(item);
            id = item.Kind switch
            {
                "file" => await ImportFileAsync(item, bytes, chooseLanguage),
                "text" => await ImportPastedTextCoreAsync(Encoding.UTF8.GetString(bytes)),
                "url" => await ImportArticleCoreAsync(WebArticles.Address(Encoding.UTF8.GetString(bytes))
                    ?? throw new InvalidDataException("Збережене вебпосилання недійсне.")),
                _ => throw new InvalidDataException("Вхідний матеріал пошкоджено."),
            };
        }
        if (id is not null) await RemoveShareItemAsync(item);
        return id;
    }

    private Task RemoveShareItemAsync(ShareInboxItem item) => Task.Run(() => ShareInbox.Remove(item));
}
