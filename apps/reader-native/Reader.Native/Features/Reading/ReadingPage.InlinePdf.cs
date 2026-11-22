using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool inlinePdf;
    private int inlinePdfRequest;
    private (string Document, int Page)? inlinePdfAddress;

    private async void ToggleInlinePdfClicked(object? sender, EventArgs args)
    {
        if (inlinePdf)
        {
            ClearInlinePdf();
            return;
        }

        inlinePdf = true;
        await ShowInlinePdfAsync();
    }

    private void ClearInlinePdf()
    {
        inlinePdf = false;
        inlinePdfRequest++;
        inlinePdfAddress = null;
        SourceText.IsVisible = DetachedPdfText.IsVisible = true;
        InlinePdfImage.IsVisible = DetachedInlinePdfImage.IsVisible = false;
        InlinePdfImage.Source = DetachedInlinePdfImage.Source = null;
        InlinePdfToggle.Text = "Сторінка PDF";
    }

    private async Task ShowInlinePdfAsync()
    {
        var session = AppState.Session;
        if (!inlinePdf || session.Document is not { Format: "pdf" } document)
        {
            ClearInlinePdf();
            return;
        }

        var request = ++inlinePdfRequest;
        try
        {
            var page = document.PdfPageStarts is not null
                ? await session.PdfReadingPageAsync(document.Id) ?? session.OriginalPageIndex()
                : session.OriginalPageIndex();
            if (request != inlinePdfRequest || !inlinePdf) return;
            if (inlinePdfAddress == (document.Id, page)) return;

            InlinePdfToggle.Text = "Завантажуємо PDF…";
            PhotoPreview preview;
#if MACCATALYST
            if (await session.OriginalFileAsync(document.Id) is { } file)
                preview = await PdfScan.PreviewAsync(file.Path, page);
            else
#endif
            {
                var fileBytes = (await session.OriginalAsync(document.Id)).Bytes.Select(value => checked((byte)value)).ToArray();
                preview = await PdfScan.PreviewAsync(fileBytes, page);
            }

            if (request != inlinePdfRequest || !inlinePdf || !readingPageActive ||
                session.Document?.Id != document.Id) return;
            var source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false));
            InlinePdfImage.Source = DetachedInlinePdfImage.Source = source;
            InlinePdfImage.IsVisible = DetachedInlinePdfImage.IsVisible = true;
            SourceText.IsVisible = DetachedPdfText.IsVisible = false;
            inlinePdfAddress = (document.Id, page);
            InlinePdfToggle.Text = "Розпізнаний текст";
        }
        catch (Exception error)
        {
            if (request != inlinePdfRequest) return;
            ClearInlinePdf();
            await DisplayAlertAsync("Сторінка PDF", error.Message, "Гаразд");
        }
    }
}
