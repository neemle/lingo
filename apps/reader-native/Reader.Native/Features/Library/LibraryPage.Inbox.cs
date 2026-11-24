using System.Text;
using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private bool pauseExternalBatch;

    private sealed record InboxEntry(ShareInboxItem? Native, IntakeItem? Core)
    {
        public string Name => Native?.Name ?? Core!.Name;
        public DateTimeOffset ReceivedAt => Native?.ReceivedAt ?? DateTimeOffset.FromUnixTimeSeconds(Core!.ReceivedAt);
        public bool CanResume => Native?.Kind != "damaged";
        public string Label => Native?.Label ?? $"{Name} · {ReceivedAt.LocalDateTime:g}";
    }

    private async Task<InboxEntry[]> ReadInboxAsync()
    {
        var native = await ShareInbox.ListAsync();
        var core = await AppState.Session.ListIntakeAsync();
        var captured = native.Select(item => item.Id.ToString("D")).ToHashSet(StringComparer.Ordinal);
        return native.Select(item => new InboxEntry(item, null))
            .Concat(core.Where(item => !captured.Contains(item.Id)).Select(item => new InboxEntry(null, item)))
            .OrderBy(item => item.ReceivedAt).ToArray();
    }

    private async Task RefreshShareInboxAsync()
    {
        try
        {
            var items = await ReadInboxAsync();
            OpenShareInbox.Text = $"Вхідні матеріали ({items.Length})";
            OpenShareInbox.IsVisible = items.Length > 0;
        }
        catch (Exception error)
        {
            ImportStatus.Text = $"Не вдалося прочитати вхідні матеріали. {error.Message}";
        }
    }

    private async void OpenShareInboxClicked(object? sender, EventArgs args)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        try
        {
            var items = await ReadInboxAsync();
            if (items.Length == 0) return;
            const string resumeAll = "Продовжити імпорт усіх";
            var recoverable = items.Where(item => item.CanResume).ToArray();
            var itemChoices = items.Select((item, index) => $"{index + 1}. {item.Label}").ToArray();
            string[] choices = recoverable.Length > 1 ? [resumeAll, .. itemChoices] : itemChoices;
            var choice = await DisplayActionSheetAsync("Вхідні матеріали", "Закрити", null, choices);
            if (choice == resumeAll)
            {
                await ImportInboxBatchAsync(recoverable);
                return;
            }
            var selected = Array.IndexOf(itemChoices, choice);
            if (selected < 0) return;
            var item = items[selected];
            const string changeLanguage = "Змінити мову оригіналу";
            string[] actions = !item.CanResume ? [] : item.Native?.SourceLanguage is null
                ? ["Продовжити імпорт"] : ["Продовжити імпорт", changeLanguage];
            var action = await DisplayActionSheetAsync(item.Name, "Скасувати", "Видалити копію", actions);
            if (action == "Видалити копію" && await DisplayAlertAsync("Видалити вхідний матеріал?", item.Name, "Видалити", "Скасувати"))
            {
                if (item.Native is { } native) await RemoveShareItemAsync(native);
                else await AppState.Session.DiscardIntakeAsync(Guid.Parse(item.Core!.Id));
            }
            if (action is "Продовжити імпорт" or changeLanguage)
                await ImportInboxBatchAsync([item], chooseLanguage: action == changeLanguage);
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

    internal Task ImportExternalBatchAsync(ShareInboxItem[] items, bool chooseLanguage = false) =>
        ImportInboxBatchAsync(items.Select(item => new InboxEntry(item, null)).ToArray(), chooseLanguage);

    private async Task ImportInboxBatchAsync(InboxEntry[] items, bool chooseLanguage = false)
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
                try
                {
                    var imported = item.Native is { } native
                        ? await ImportInboxItemAsync(native, chooseLanguage)
                        : await CompleteStagedIntakeAsync(item.Core!);
                    lastDocument = imported ?? lastDocument;
                }
                catch (Exception error) { await DisplayAlertAsync(item.Name, error.Message, "Гаразд"); }
            }
            if (!pauseExternalBatch && lastDocument is not null) await AppState.OpenAsync(lastDocument);
        }
        finally
        {
            PauseExternalBatch.IsVisible = false;
            importing = false;
            ImportStatus.Text = pauseExternalBatch
                ? "Імпорт призупинено. Продовжити можна у вхідних матеріалах." : "";
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
        var session = AppState.Session;
        var isPdf = item.Kind == "file" && Path.GetExtension(item.Name).Equals(".pdf", StringComparison.OrdinalIgnoreCase);
        if (item.ScanPdf && !isPdf) throw new InvalidDataException("Виберіть файл PDF для розпізнавання.");
        string? id;
        if (await session.FindIntakeAsync(item.Id) is { } staged)
            id = await CompleteStagedIntakeAsync(staged, item, chooseLanguage);
#if MACCATALYST
        else if (isPdf)
        {
            ImportStatus.Text = "Зберігаємо оригінал PDF…";
            var intake = await session.StagePdfIntakeAsync(item.Id, item.Name, ShareInbox.PayloadPath(item));
            id = await CompleteStagedIntakeAsync(intake, item, chooseLanguage);
        }
#else
        else if (isPdf)
            id = await ImportLegacyPdfAsync(item, await ShareInbox.ReadAsync(item), chooseLanguage);
#endif
        else
        {
            var bytes = await ShareInbox.ReadAsync(item);
            id = item.Kind switch
            {
                "file" => await CompleteStagedIntakeAsync(
                    await session.StageFileIntakeAsync(item.Id, item.Name, bytes), item, chooseLanguage),
                "text" => await ImportPastedTextCoreAsync(Encoding.UTF8.GetString(bytes), item),
                "url" => await ImportArticleCoreAsync(WebArticles.Address(Encoding.UTF8.GetString(bytes))
                    ?? throw new InvalidDataException("Збережене вебпосилання недійсне."), item),
                _ => throw new InvalidDataException("Вхідний матеріал пошкоджено."),
            };
        }
        if (id is not null) await RemoveShareItemAsync(item);
        return id;
    }

    private async Task RemoveShareItemAsync(ShareInboxItem item)
    {
        await AppState.Session.DiscardIntakeAsync(item.Id);
        await Task.Run(() => ShareInbox.Remove(item));
    }
}
