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
                if (ImportedPdf(snapshot, job) is null) found = true;
                else job.Complete();
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
                    await Task.Run(job.Complete);
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

    private async Task<string?> RunPdfOcrAsync(PdfOcrCheckpoint job, byte[]? source = null)
    {
        using var active = new CancellationTokenSource();
        var cancellation = active.Token;
        pdfCancellation = active;
        ResumePdf.IsVisible = false;
        PausePdf.IsVisible = PausePdf.IsEnabled = true;
        SetPdfStatus($"{job.Name} · відкриваємо PDF…");
        try
        {
            var session = AppState.Session;
            // A crash after the core commit can leave a completed checkpoint on disk.
            var imported = ImportedPdf(session.Snapshot, job);
            if (imported is not null)
            {
                await Task.Run(job.Complete);
                SetPdfStatus("");
                return imported.Id;
            }

            var progress = new Progress<int>(saved =>
            {
                if (pdfCancellation == active && PausePdf.IsEnabled)
                    SetPdfStatus($"{job.Name} · збережено сторінок OCR {saved}/{job.MissingPages.Length}");
            });
            var (bytes, lines) = await Task.Run(async () =>
            {
                var bytes = source ?? job.ReadOriginal();
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
                    await PdfScan.RecognizeAsync(bytes, job.Language, job.Pages, pending.ToArray(), (page, lines) =>
                    {
                        Include(page, lines);
                        job.SavePage(page, lines);
                        report.Report(saved.Count);
                    }, cancellation);
                }
                cancellation.ThrowIfCancellationRequested();
                return (bytes, saved.Values.SelectMany(lines => lines).ToArray());

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
            await session.ImportMixedPdfAsync(job.Name, bytes, job.Language, lines, job.Pages);
            if (session.Error.Length > 0) throw new InvalidDataException(session.Error);
            if (session.Document is not { } document || document.SourceHash != job.Hash)
                throw new InvalidDataException("Імпорт PDF не підтверджено.");
            await Task.Run(job.Complete);
            SetPdfStatus("");
            return document.Id;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            SetPdfStatus($"{job.Name} · розпізнавання призупинено. Прогрес збережено.");
            return null;
        }
        catch
        {
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
