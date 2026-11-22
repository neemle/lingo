using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private CancellationTokenSource? pdfCancellation;

    private async Task RefreshPendingPdfAsync()
    {
        var snapshot = AppState.Session.Snapshot;
        var hasPending = await Task.Run(() =>
        {
            var found = false;
            foreach (var job in PdfOcrCheckpoint.List())
            {
                if (ImportedPdf(snapshot, job) is not { } document) found = true;
                else job.Complete(document.Id);
            }
            return found;
        });
        ResumePdf.IsVisible = hasPending && pdfCancellation is null;
    }

    private async void ResumePdfClicked(object? sender, EventArgs args)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        importing = true;
        try
        {
            var jobs = await Task.Run(PdfOcrCheckpoint.List);
            if (jobs.Length == 0)
            {
                ResumePdf.IsVisible = false;
                return;
            }
            var job = jobs[0];
            if (jobs.Length > 1)
            {
                var choices = jobs.Select((entry, index) =>
                    $"{index + 1}. {entry.Name} · {entry.SavedPages}/{entry.MissingPages.Length}").ToArray();
                var choice = await DisplayActionSheetAsync("Продовжити розпізнавання PDF", "Скасувати", null, choices);
                var index = Array.IndexOf(choices, choice);
                if (index < 0) return;
                job = jobs[index];
            }
            var action = await DisplayActionSheetAsync(job.Name, "Скасувати", "Видалити чернетку", "Продовжити");
            if (action == "Видалити чернетку")
            {
                if (await DisplayAlertAsync("Видалити чернетку OCR?", job.Name, "Видалити", "Скасувати"))
                {
                    await Task.Run(job.Cancel);
                    await RefreshPendingPdfAsync();
                }
                return;
            }
            if (action != "Продовжити") return;
            if (await RunPdfOcrAsync(job) is { } id) await AppState.OpenAsync(id);
        }
        catch (Exception error) { await DisplayAlertAsync("Розпізнавання PDF", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            DesktopImport.Gate.Release();
        }
    }

    private void PausePdfClicked(object? sender, EventArgs args)
    {
        if (pdfCancellation is not { } active) return;
        PausePdf.IsEnabled = false;
        SetPdfStatus("Призупиняємо розпізнавання…");
        active.Cancel();
    }

#if MACCATALYST
    private async Task<string?> ImportPdfForReadingAsync(ShareInboxItem item, bool chooseLanguage)
    {
        var session = AppState.Session;
        await session.WaitUntilIdleAsync();
        ImportStatus.Text = "Зберігаємо оригінал PDF…";
        var intake = await session.StagePdfIntakeAsync(item.Id, item.Name, ShareInbox.PayloadPath(item));
        if (intake.DocumentId is { } completed) return completed;
        var source = await session.IntakeFileAsync(intake.Id);
        ImportStatus.Text = "Читаємо текстовий шар PDF…";
        var pageTexts = await PdfScan.ExtractTextAsync(source.Path);
        var language = await ResolveImportLanguageAsync(item, chooseLanguage, () =>
        {
            if (item.ScanPdf) return Task.FromResult<string?>(null);
            var excerpt = string.Join('\n', pageTexts.Where(text => !string.IsNullOrWhiteSpace(text)).Take(4)
                .Select(text => text[..Math.Min(text.Length, 2000)]));
            return session.DetectLanguageAsync(excerpt);
        });
        if (language is null) return null;

        await session.WaitUntilIdleAsync();
        ImportStatus.Text = "Зберігаємо документ…";
        await session.CompletePdfIntakeAsync(intake.Id, language, pageTexts);
        if (session.Error.Length != 0) throw new InvalidDataException(session.Error);
        return session.Document?.Id;
    }
#endif

    private async Task<string?> RunPdfOcrAsync(PdfOcrCheckpoint job, byte[]? source = null, string[]? pageTexts = null)
    {
        using var active = new CancellationTokenSource();
        var cancellation = active.Token;
        pdfCancellation = active;
        ResumePdf.IsVisible = false;
        PausePdf.IsVisible = PausePdf.IsEnabled = true;
        SetPdfStatus($"{job.Name} · відкриваємо PDF…");
        var started = false;
        try
        {
            var session = AppState.Session;
            // A crash after the core commit can leave a completed checkpoint on disk.
            var imported = ImportedPdf(session.Snapshot, job);
            if (imported is not null)
            {
                await Task.Run(() => job.Complete(imported.Id));
                SetPdfStatus("");
                return imported.Id;
            }
            await Task.Run(job.Start);
            started = true;

            var progress = new Progress<int>(saved =>
            {
                if (pdfCancellation == active && PausePdf.IsEnabled)
                    SetPdfStatus($"{job.Name} · збережено сторінок OCR {saved}/{job.MissingPages.Length}");
            });
            var completed = await Task.Run(async () =>
            {
#if MACCATALYST
                var file = source is null && job.UsesFile ? job.ReadOriginalPath() : null;
                var bytes = file is null ? source ?? job.ReadOriginal() : null;
#else
                var bytes = source ?? job.ReadOriginal();
#endif
                var saved = new SortedDictionary<int, OcrLine[]>();
                var pending = new List<int>();
                var lineCount = 0;
                var wordCount = 0;
                foreach (var page in job.MissingPages)
                {
                    cancellation.ThrowIfCancellationRequested();
                    if (job.ReadPage(page) is { } lines) Include(page, lines);
                    else pending.Add(page);
                }
                var report = (IProgress<int>)progress;
                report.Report(saved.Count);
                if (pending.Count > 0)
                {
                    var pendingPages = pending.ToArray();
#if MACCATALYST
                    if (file is not null)
                        await PdfScan.RecognizeAsync(file, job.Language, job.Pages, pendingPages, SavePage, cancellation);
                    else
#endif
                        await PdfScan.RecognizeAsync(bytes!, job.Language, job.Pages, pendingPages, SavePage, cancellation);
                }
                cancellation.ThrowIfCancellationRequested();
                var recognizedLines = saved.Values.SelectMany(lines => lines).ToArray();
#if MACCATALYST
                return (Bytes: bytes, Lines: recognizedLines, Path: file);
#else
                return (Bytes: bytes, Lines: recognizedLines);
#endif

                void SavePage(int page, OcrLine[] lines)
                {
                    Include(page, lines);
                    job.SavePage(page, lines);
                    report.Report(saved.Count);
                }

                void Include(int page, OcrLine[] lines)
                {
                    lineCount += lines.Length;
                    wordCount += lines.Sum(line => line.Words?.Length ?? 0);
                    if (lineCount > 8192 || wordCount > 65_536)
                        throw new InvalidDataException("Розпізнаний текст PDF перевищує допустимий розмір.");
                    saved.Add(page, lines);
                }
            }, cancellation);

            await session.WaitUntilIdleAsync();
            cancellation.ThrowIfCancellationRequested();
            PausePdf.IsVisible = PausePdf.IsEnabled = false;
            SetPdfStatus($"{job.Name} · зберігаємо документ…");
#if MACCATALYST
            if (completed.Path is { } path)
            {
                pageTexts ??= await PdfScan.ExtractTextAsync(path, cancellation);
                if (pageTexts.Length != job.Pages)
                    throw new InvalidDataException("Кількість сторінок PDF змінилася.");
                await session.ImportPdfFileAsync(job.Name, path, job.Language, pageTexts, completed.Lines);
            }
            else
#endif
                await session.ImportMixedPdfAsync(job.Name, completed.Bytes!, job.Language, completed.Lines, job.Pages);
            if (session.Error.Length > 0) throw new InvalidDataException(session.Error);
            if (session.Document is not { } document || document.SourceHash != job.Hash)
                throw new InvalidDataException("Імпорт PDF не підтверджено.");
            await Task.Run(() => job.Complete(document.Id));
            SetPdfStatus("");
            return document.Id;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            if (started) await Task.Run(job.Pause);
            SetPdfStatus($"{job.Name} · розпізнавання призупинено. Прогрес збережено.");
            return null;
        }
        catch
        {
            if (started) await Task.Run(job.Fail);
            SetPdfStatus($"{job.Name} · збережений прогрес доступний у бібліотеці.");
            throw;
        }
        finally
        {
            PausePdf.IsVisible = false;
            pdfCancellation = null;
            await RefreshPendingPdfAsync();
        }
    }

    private static Document? ImportedPdf(Snapshot snapshot, PdfOcrCheckpoint job) =>
        snapshot.Documents.FirstOrDefault(document => document.SourceHash == job.Hash
            && document.OcrPdfPages == job.Pages && document.PdfPageStarts is not null);

    private void SetPdfStatus(string text)
    {
        PdfStatus.Text = text;
        PdfStatus.IsVisible = text.Length > 0;
    }
}
