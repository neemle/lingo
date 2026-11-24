using System.Globalization;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

internal sealed class OriginalPage : ContentPage
{
    private readonly Image image = new() { Aspect = Aspect.AspectFit };
    private static readonly double[] ZoomLevels = [1, 1.25, 1.5, 2, 2.5, 3];
    private readonly ScrollView viewport = new() { Orientation = ScrollOrientation.Both };
    private readonly Grid pageCanvas = new();
    private readonly Grid surface = new() { HorizontalOptions = LayoutOptions.Center, VerticalOptions = LayoutOptions.Center };
    private readonly Button zoomOut = new() { Text = "−" };
    private readonly Button zoomIn = new() { Text = "+" };
    private readonly Button fit = new() { Text = "Вмістити" };
    private int zoomLevel;
    private readonly GraphicsView overlay = new() { InputTransparent = true, IsVisible = false };
    private readonly Label status = new() { VerticalOptions = LayoutOptions.Center };
    private readonly Button previous = new() { Text = "←" };
    private readonly Button next = new() { Text = "→" };
    private readonly Button jump = new() { Text = "До сторінки…" };
    private readonly Button toggle = new();
    private readonly Button readPage = new() { Text = "Читати й перекладати" };
    private readonly Button wordCard = new() { Text = "Картка слова" };
    private readonly Button reviewOcrWord = new() { Text = "Перевірити слово з Qwen", IsVisible = false };
    private readonly Button proofreadPhoto = new() { Text = "Звірити текст фото" };
    private readonly Func<int, Task<PhotoPreview>> loadPage;
    private readonly Func<TextSelection, Task> showWord;
    private readonly Func<int, TextSelection, Task>? showDetachedWord;
    private readonly Func<int, CancellationToken, Func<Task>, Task>? recognizeWindow;
    private readonly Grid editor;
    private CancellationTokenSource? pageOcr;
    private bool visible;
    private OcrLine[] lines;
    private DetachedPdfOcrPage? detachedOcr;
    private readonly ReaderSession session;
    private readonly string documentId;
    private readonly Button lineButton = new() { Text = "Розпізнані рядки" };
    private int selectedLineChoice = -1;
    private readonly Button edit = new() { Text = "Виправити" };
    private readonly Button suggest = new() { Text = "Підказки" };
    private readonly Button reviewOcrLine = new() { Text = "Звірити рядок із Qwen" };
    private readonly Button reviewOcrPage = new() { Text = "Звірити сторінку із Qwen" };
    private readonly OcrPageReviewFlow ocrPageReview;
    private readonly Button reset = new() { Text = "Скинути" };
    private readonly Label editStatus = new();
    private int[] currentLineIndices = [];
    private bool editing;
    private bool leavingOriginal;
    private bool choosingPage;
    private bool choosingLine;
    private readonly int pageCount;
    private int pageIndex;
    private bool loading;
    private (int Start, int End)? focus;
    private int previewWidth;
    private int previewHeight;
    private (int LineIndex, OcrWord Word, TextSelection Selection)? activeWord;
    private SourceRanges? wordRanges;

    internal OriginalPage(string filename, int pageCount, int initialPage, bool isPdf,
        Func<int, Task<PhotoPreview>> loadPage, string documentId, ReaderSession session, OcrLine[] lines,
        TextSelection? selection, Func<TextSelection, Task> showWord, OcrPageReviewFlow ocrPageReview,
        Func<int, CancellationToken, Func<Task>, Task>? recognizeWindow = null,
        Func<int, TextSelection, Task>? showDetachedWord = null)
    {
        this.loadPage = loadPage;
        this.showWord = showWord;
        this.ocrPageReview = ocrPageReview;
        this.showDetachedWord = showDetachedWord;
        this.recognizeWindow = recognizeWindow;
        this.lines = lines;
        this.documentId = documentId;
        this.session = session;
        this.pageCount = pageCount;
        focus = selection is not null ? (selection.StartUtf16, selection.StartUtf16 + selection.LengthUtf16)
            : SentenceRange();
        pageIndex = selection is null ? initialPage
            : lines.FirstOrDefault(line => Overlaps(line.SourceSpan, focus))?.Page ?? initialPage;
        Title = isPdf ? "Вихідний PDF" : "Вихідна фотографія";
        SemanticProperties.SetDescription(image, filename);
        foreach (var (button, description) in new[]
        {
            (previous, "Попередня сторінка"),
            (next, "Наступна сторінка"),
            (jump, "Перейти за номером сторінки PDF"),
            (zoomOut, "Зменшити масштаб оригіналу"),
            (zoomIn, "Збільшити масштаб оригіналу"),
            (fit, "Вмістити оригінал у вікно"),
            (readPage, "Відкрити цю сторінку в читачі"),
            (wordCard, "Показати словникову картку виділення"),
            (reviewOcrWord, "Звірити розпізнане слово локальною моделлю"),
            (proofreadPhoto, "Звірити розпізнаний текст фото"),
            (lineButton, "Вибрати розпізнаний рядок"),
            (edit, "Змінити розпізнаний текст рядка"),
            (suggest, "Переглянути підказки для розпізнаного рядка"),
            (reviewOcrLine, "Звірити рядок локальною моделлю"),
            (reset, "Відновити початковий результат OCR для рядка"),
        })
        {
            ReadingPage.DescribeButton(button, description);
        }
        zoomOut.Clicked += (_, _) => SetZoom(zoomLevel - 1);
        zoomIn.Clicked += (_, _) => SetZoom(zoomLevel + 1);
        fit.Clicked += (_, _) => SetZoom(0);
        previous.IsVisible = next.IsVisible = jump.IsVisible = pageCount > 1;
        jump.Clicked += async (_, _) => await JumpPageAsync();
        previous.Clicked += async (_, _) => await LoadAsync(pageIndex - 1);
        next.Clicked += async (_, _) => await LoadAsync(pageIndex + 1);
        overlay.IsVisible = lines.Any(line => Overlaps(line.SourceSpan, focus));
        toggle.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName == nameof(Button.Text))
                ReadingPage.DescribeButton(toggle, OverlayLabel());
        };
        toggle.Text = OverlayLabel();
        toggle.IsVisible = lines.Length > 0;
        toggle.Clicked += (_, _) =>
        {
            overlay.IsVisible = !overlay.IsVisible;
            overlay.Invalidate();
            toggle.Text = OverlayLabel();
        };
        wordCard.IsVisible = lines.Any(line => line.Words is { Length: > 0 });
        wordCard.Clicked += async (_, _) => await OpenWordAsync();
        reviewOcrWord.Clicked += async (_, _) => await ReviewOcrWordAsync();
        readPage.IsVisible = isPdf;
        readPage.Clicked += async (_, _) => await ReadPageAsync();
        proofreadPhoto.IsVisible = !isPdf && lines.Length > 0;
        proofreadPhoto.Clicked += async (_, _) => await ProofreadPhotoAsync();
        var controls = new FlexLayout { Wrap = Microsoft.Maui.Layouts.FlexWrap.Wrap };
        foreach (var control in new View[] { previous, next, jump, readPage, zoomOut, zoomIn, fit, toggle,
            wordCard, reviewOcrWord, proofreadPhoto, status })
        {
            control.Margin = new Thickness(0, 0, 8, 5);
            controls.Children.Add(control);
        }
        lineButton.Clicked += async (_, _) => await ChooseLineAsync();
        edit.Clicked += async (_, _) => await CorrectAsync(LineAction.Edit);
        suggest.Clicked += async (_, _) => await CorrectAsync(LineAction.Suggest);
        reviewOcrLine.Clicked += async (_, _) => await ReviewOcrLineAsync();
        reviewOcrPage.Clicked += async (_, _) => await ReviewOcrPageAsync();
        reset.Clicked += async (_, _) => await CorrectAsync(LineAction.Reset);
        var editButtons = new FlexLayout { Wrap = Microsoft.Maui.Layouts.FlexWrap.Wrap };
        foreach (var button in new[] { suggest, reviewOcrLine, reviewOcrPage, edit, reset })
        {
            button.Margin = new Thickness(0, 0, 8, 5);
            editButtons.Children.Add(button);
        }
        editor = new Grid { RowSpacing = 5, RowDefinitions = { new(GridLength.Auto), new(GridLength.Auto) } };
        editor.Add(lineButton);
        editor.Add(editButtons, 0, 1);

        var content = new Grid
        {
            Padding = 16,
            RowSpacing = 8,
            RowDefinitions = new RowDefinitionCollection
            {
                new RowDefinition(GridLength.Auto),
                new RowDefinition(GridLength.Star),
                new RowDefinition(GridLength.Auto),
                new RowDefinition(GridLength.Auto),
            },
        };
        surface.Children.Add(image);
        surface.Children.Add(overlay);
        pageCanvas.Children.Add(surface);
        viewport.Content = pageCanvas;
        viewport.SizeChanged += (_, _) => ResizePage();
        var tap = new TapGestureRecognizer();
        tap.Tapped += (_, args) => SelectOcrAt(surface, args);
        surface.GestureRecognizers.Add(tap);
        Grid.SetRow(viewport, 1);
        content.Children.Add(controls);
        content.Children.Add(viewport);
        Grid.SetRow(editor, 2);
        Grid.SetRow(editStatus, 3);
        content.Children.Add(editor);
        content.Children.Add(editStatus);
        editStatus.IsVisible = editor.IsVisible = lines.Length != 0;
        UpdateEditButtons();
        Content = content;
    }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        visible = true;
        if (image.Source is null) await LoadAsync(pageIndex);
        else if (!editing) QueuePageOcr(pageIndex);
    }

    protected override void OnDisappearing()
    {
        visible = false;
        ocrPageReview.CancelInference();
        pageOcr?.Cancel();
        base.OnDisappearing();
    }

    private async Task JumpPageAsync()
    {
        if (!jump.IsEnabled || Navigation.NavigationStack.LastOrDefault() != this) return;
        choosingPage = true;
        UpdateEditButtons();
        try
        {
            var destination = await PdfPagePrompt.ChooseAsync(this, pageIndex, pageCount);
            if (destination is int page && page != pageIndex && Navigation.NavigationStack.LastOrDefault() == this)
                await LoadAsync(page);
        }
        finally
        {
            choosingPage = false;
            UpdateEditButtons();
        }
    }

    private async Task LoadAsync(int index)
    {
        if (loading || editing || leavingOriginal || index < 0 || index >= pageCount) return;
        pageOcr?.Cancel();
        loading = true;
        status.Text = "Завантаження сторінки…";
        UpdateEditButtons();
        try
        {
            var changingPage = index != pageIndex;
            var preview = await loadPage(index);
            var cached = recognizeWindow is null ? null : await session.DetachedPdfOcrAsync(documentId, index);
            if (index != pageIndex || detachedOcr is not null || cached is not null)
            {
                activeWord = null;
                focus = cached is null ? SentenceRange() : null;
            }
            pageIndex = index;
            image.Source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false));
            previewWidth = preview.Width;
            previewHeight = preview.Height;
            ResizePage();
            ApplyOcrLines(cached);
            status.Text = pageCount == 1 ? "" : $"{pageIndex + 1} / {pageCount}";
            if (changingPage && visible && session.Document is { } document && document.Id == documentId &&
                HasPhysicalPageAddress(document))
                await session.SetPdfReadingPageAsync(documentId, index);
            QueuePageOcr(index);
            if (changingPage && (viewport.ScrollX > 0 || viewport.ScrollY > 0))
                await viewport.ScrollToAsync(0, 0, animated: false);
        }
        catch (Exception error) { status.Text = error.Message; }
        finally
        {
            loading = false;
            UpdateEditButtons();
        }
    }

    private void SetZoom(int level)
    {
        if (loading || image.Source is null) return;
        zoomLevel = Math.Clamp(level, 0, ZoomLevels.Length - 1);
        ResizePage();
        UpdateEditButtons();
    }

    private void ResizePage()
    {
        var width = viewport.Width;
        var height = viewport.Height;
        if (width <= 0 || height <= 0 || previewWidth <= 0 || previewHeight <= 0) return;

        var scale = Math.Min(width / previewWidth, height / previewHeight) * ZoomLevels[zoomLevel];
        surface.WidthRequest = Math.Floor(previewWidth * scale);
        surface.HeightRequest = Math.Floor(previewHeight * scale);
        pageCanvas.WidthRequest = Math.Max(width, surface.WidthRequest);
        pageCanvas.HeightRequest = Math.Max(height, surface.HeightRequest);
        overlay.Invalidate();
    }

    private void QueuePageOcr(int index)
    {
        if (!visible || recognizeWindow is null) return;
        pageOcr?.Cancel();
        var run = new CancellationTokenSource();
        pageOcr = run;
        _ = RecognizeWindowAsync(index, run);
    }

    private async Task RecognizeWindowAsync(int index, CancellationTokenSource run)
    {
        try
        {
            await recognizeWindow!(index, run.Token, async () =>
            {
                if (!visible || run.IsCancellationRequested || session.Busy) return;
                await session.RefreshAsync();
                if (!visible || run.IsCancellationRequested || pageIndex != index ||
                    session.Document?.Id != documentId)
                    return;
                var cached = await session.DetachedPdfOcrAsync(documentId, index);
                if (!visible || run.IsCancellationRequested || pageIndex != index || editing ||
                    session.Document?.Id != documentId)
                    return;
                activeWord = null;
                focus = cached is null ? SentenceRange() : null;
                ApplyOcrLines(cached);
            });
        }
        catch (OperationCanceledException) when (run.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (visible && !run.IsCancellationRequested) status.Text = error.Message;
        }
        finally
        {
            if (pageOcr == run) pageOcr = null;
            run.Dispose();
        }
    }

    private void ApplyOcrLines(DetachedPdfOcrPage? cached)
    {
        var enteringDetachedOcr = detachedOcr is null && cached is not null;
        detachedOcr = cached;
        lines = cached?.Lines ?? session.Document?.OcrLines ?? [];
        toggle.IsVisible = editor.IsVisible = editStatus.IsVisible = lines.Any(line => line.Page == pageIndex);
        wordCard.IsVisible = (cached is null || showDetachedWord is not null) &&
            lines.Any(line => line.Page == pageIndex && line.Words?.Any(word => word.SourceSpan is not null) == true);
        if (enteringDetachedOcr) overlay.IsVisible = true;
        if (!toggle.IsVisible) overlay.IsVisible = false;
        toggle.Text = OverlayLabel();
        RebuildLineChoices();
        UpdateOverlay();
    }

    private string OverlayLabel() => overlay.IsVisible ? "Приховати області тексту" : "Показати області тексту";

    private (int Start, int End)? SentenceRange() => session.Sentence is { } sentence
        ? (sentence.StartUtf16, sentence.EndUtf16) : null;

    private static bool Overlaps(OcrSpan? span, (int Start, int End)? focus) =>
        span is { } source && focus is { } range && source.StartUtf16 < range.End && source.EndUtf16 > range.Start;

    internal static RectF AspectFit(RectF area, int width, int height)
    {
        if (width <= 0 || height <= 0 || area.Width <= 0 || area.Height <= 0) return default;
        var scale = Math.Min(area.Width / width, area.Height / height);
        var fittedWidth = width * scale;
        var fittedHeight = height * scale;
        return new RectF(area.X + (area.Width - fittedWidth) / 2, area.Y + (area.Height - fittedHeight) / 2,
            fittedWidth, fittedHeight);
    }

    private void SelectOcrAt(View surface, TappedEventArgs args)
    {
        if (loading || editing || leavingOriginal || choosingLine || session.Busy || args.GetPosition(surface) is not { } point) return;
        var fitted = AspectFit(new RectF(0, 0, (float)surface.Width, (float)surface.Height), previewWidth, previewHeight);
        if (fitted.Width <= 0 || fitted.Height <= 0) return;
        var x = (point.X - fitted.X) / fitted.Width;
        var y = (point.Y - fitted.Y) / fitted.Height;
        if (!(x >= 0 && x < 1 && y >= 0 && y < 1)) return;

        for (var choice = 0; choice < currentLineIndices.Length; choice++)
        {
            var index = currentLineIndices[choice];
            var line = lines[index];
            foreach (var word in line.Words ?? [])
            {
                if (x < word.X || x >= word.X + word.Width || y < word.Y || y >= word.Y + word.Height ||
                    CurrentWord(index, word) is not { } selected) continue;
                SelectLine(choice, highlight: false);
                activeWord = (index, word, selected);
                editStatus.Text = "";
                focus = (selected.StartUtf16, selected.StartUtf16 + selected.LengthUtf16);
                overlay.IsVisible = true;
                toggle.Text = OverlayLabel();
                UpdateOverlay();
                UpdateEditButtons();
                return;
            }

            if (detachedOcr is not null && x >= line.X && x < line.X + line.Width &&
                y >= line.Y && y < line.Y + line.Height)
            {
                SelectLine(choice, highlight: false);
                activeWord = null;
                focus = line.SourceSpan is { } span ? (span.StartUtf16, span.EndUtf16) : null;
                overlay.IsVisible = true;
                toggle.Text = OverlayLabel();
                UpdateOverlay();
                UpdateEditButtons();
                return;
            }
        }
    }

    private TextSelection? CurrentWord(int index, OcrWord word)
    {
        if (session.Document is not { } document || document.Id != documentId ||
            word.SourceSpan is not { } span || span.StartUtf16 < 0 || span.EndUtf16 <= span.StartUtf16)
            return null;

        var current = detachedOcr?.Lines ?? document.OcrLines;
        if (current is null || index < 0 || index >= current.Length || current[index].Page != pageIndex ||
            current[index].Words?.Contains(word) != true) return null;

        var length = span.EndUtf16 - span.StartUtf16;
        if (detachedOcr is not null)
        {
            var line = current[index];
            if (showDetachedWord is null || line.SourceSpan is not { } lineSpan) return null;
            var reading = (line.Correction ?? line.Text).Trim();
            var start = span.StartUtf16 - lineSpan.StartUtf16;
            return start >= 0 && start <= reading.Length - length
                ? new TextSelection(span.StartUtf16, length, reading.Substring(start, length)) : null;
        }

        if (span.EndUtf16 > document.Text.Length) return null;
        if (wordRanges is null || wordRanges.Text != document.Text) wordRanges = new SourceRanges(document.Text);
        var selected = wordRanges.Selection(span.StartUtf16, length);
        return selected is not null && selected.StartUtf16 == span.StartUtf16 &&
            selected.LengthUtf16 == length
            ? selected : null;
    }

    private async Task OpenWordAsync()
    {
        if (loading || editing || leavingOriginal || session.Busy || activeWord is not { } active ||
            Navigation.NavigationStack.LastOrDefault() != this) return;
        if (CurrentWord(active.LineIndex, active.Word) != active.Selection)
        {
            activeWord = null;
            focus = SentenceRange();
            UpdateOverlay();
            UpdateEditButtons();
            return;
        }

        leavingOriginal = true;
        UpdateEditButtons();
        try
        {
            if (detachedOcr is { } cached && showDetachedWord is { } showDetached)
            {
                pageOcr?.Cancel();
                await session.FocusDetachedPdfOcrWordAsync(documentId, pageIndex, cached.Revision, active.Selection);
                if (session.Error.Length > 0)
                {
                    status.Text = session.Error;
                    activeWord = null;
                    focus = null;
                    ApplyOcrLines(await session.DetachedPdfOcrAsync(documentId, pageIndex));
                    return;
                }
                if (Navigation.NavigationStack.LastOrDefault() != this || session.Document?.Id != documentId) return;
                await Navigation.PopAsync();
                await showDetached(pageIndex, active.Selection);
            }
            else
            {
                await Navigation.PopAsync();
                if (!session.Busy && CurrentWord(active.LineIndex, active.Word) == active.Selection)
                    await showWord(active.Selection);
            }
        }
        catch (Exception error) { editStatus.Text = error.Message; }
        finally
        {
            leavingOriginal = false;
            UpdateEditButtons();
        }
    }

    private async Task ReadPageAsync()
    {
        if (!readPage.IsEnabled || session.Busy || session.Document is not { } document || document.Id != documentId ||
            Navigation.NavigationStack.LastOrDefault() != this)
            return;

        leavingOriginal = true;
        UpdateEditButtons();
        try
        {
            var opened = true;
            if (HasPhysicalPageAddress(document))
            {
                pageOcr?.Cancel();
                await session.OpenPdfReadingPageAsync(documentId, revisionId: null, page: pageIndex);
            }
            else if (DocumentPages.SentencePosition(document, pageIndex) is int position)
            {
                if (document.CurrentRevisionId is { } revision)
                    opened = await session.OpenPdfReadingPageAsync(documentId, revision, pageIndex);
                else
                    await session.SelectSentenceAsync(position);
            }
            else return;
            if (session.Error.Length > 0)
            {
                status.Text = session.Error;
                return;
            }
            if (opened && session.Document?.Id == documentId &&
                Navigation.NavigationStack.LastOrDefault() == this)
                await Navigation.PopAsync();
        }
        catch (Exception error) { status.Text = error.Message; }
        finally
        {
            leavingOriginal = false;
            UpdateEditButtons();
        }
    }

    private bool HasPhysicalPageAddress(Document document)
    {
#if MACCATALYST
        return document.Format == "pdf" && document.PdfPageStarts is { } starts && pageIndex < starts.Length;
#else
        return false;
#endif
    }

    private void UpdateOverlay()
    {
        overlay.Drawable = new LineOverlay(previewWidth, previewHeight,
            lines.Where(line => line.Page == pageIndex).ToArray(), focus);
        overlay.Invalidate();
    }

    private async Task ChooseLineAsync()
    {
        if (!lineButton.IsEnabled || Navigation.NavigationStack.LastOrDefault() != this) return;
        choosingLine = true;
        UpdateEditButtons();
        try
        {
            var currentPage = pageIndex;
            var offered = currentLineIndices.Select(index => lines[index]).ToArray();
            var selected = await NativeChoices.ChooseAsync(this, "Розпізнані рядки", null,
                offered.Select((line, choice) => (choice.ToString(CultureInfo.InvariantCulture), LineLabel(line, choice))),
                selectedLineChoice.ToString(CultureInfo.InvariantCulture));
            if (pageIndex != currentPage || !int.TryParse(selected, CultureInfo.InvariantCulture, out var choice) ||
                choice < 0 || choice >= offered.Length || choice >= currentLineIndices.Length)
                return;

            var current = lines[currentLineIndices[choice]];
            var original = offered[choice];
            if (current.Text == original.Text && current.Correction == original.Correction &&
                current.SourceSpan == original.SourceSpan)
                SelectLine(choice, highlight: true);
        }
        finally
        {
            choosingLine = false;
            UpdateEditButtons();
        }
    }

    private void RebuildLineChoices()
    {
        var selected = LineIndex();
        currentLineIndices = Enumerable.Range(0, lines.Length).Where(index => lines[index].Page == pageIndex).ToArray();
        SelectLine(currentLineIndices.Length == 0 ? -1 : Math.Max(0, Array.IndexOf(currentLineIndices, selected)),
            highlight: false);
        UpdateEditButtons();
    }

    private void SelectLine(int choice, bool highlight)
    {
        selectedLineChoice = choice;
        var index = LineIndex();
        lineButton.Text = index < 0 ? "Розпізнані рядки" : LineLabel(lines[index], choice);
        if (!highlight || index < 0 || lines[index].SourceSpan is not { } span) return;

        activeWord = null;
        focus = (span.StartUtf16, span.EndUtf16);
        overlay.IsVisible = true;
        toggle.Text = OverlayLabel();
        UpdateOverlay();
    }

    private static string LineLabel(OcrLine line, int choice)
    {
        var text = new StringInfo(line.Correction ?? line.Text);
        var preview = text.LengthInTextElements > 52 ? text.SubstringByTextElements(0, 52) + "…" : text.String;
        return $"{choice + 1}. {preview}";
    }

    private int LineIndex() => selectedLineChoice >= 0 && selectedLineChoice < currentLineIndices.Length
        ? currentLineIndices[selectedLineChoice] : -1;

    private void UpdateEditButtons()
    {
        var index = LineIndex();
        var canInteract = !loading && !editing && !leavingOriginal && !choosingPage && !choosingLine;
        var canZoom = canInteract && image.Source is not null;
        zoomIn.IsEnabled = canZoom && zoomLevel + 1 < ZoomLevels.Length;
        zoomOut.IsEnabled = fit.IsEnabled = canZoom && zoomLevel > 0;
        previous.IsEnabled = canInteract && pageIndex > 0;
        next.IsEnabled = canInteract && pageIndex + 1 < pageCount;
        jump.IsEnabled = canInteract && pageCount > 1;
        lineButton.IsEnabled = canInteract && currentLineIndices.Length > 0;
        wordCard.IsEnabled = canInteract && activeWord is not null;
        reviewOcrWord.IsVisible = activeWord is { Selection.Text: var word } &&
            word.Length is >= 2 and <= 32 && word.Any(char.IsLetter) &&
            word.All(ch => char.IsLetter(ch) || char.IsAsciiDigit(ch));
        reviewOcrWord.IsEnabled = canInteract && !session.Busy;
        proofreadPhoto.IsEnabled = canInteract && !session.Busy;
        readPage.IsEnabled = readPage.IsVisible && canInteract && image.Source is not null &&
            session.Document is { } document && document.Id == documentId &&
            (HasPhysicalPageAddress(document) || DocumentPages.SentencePosition(document, pageIndex).HasValue);
        edit.IsEnabled = canInteract && index >= 0;
        suggest.IsEnabled = edit.IsEnabled;
        reviewOcrLine.IsEnabled = edit.IsEnabled && !session.Busy;
        reviewOcrPage.IsEnabled = canInteract && !session.Busy && currentLineIndices.Length > 0;
        var revision = detachedOcr?.Revision ?? session.Document?.CurrentRevisionId;
        reviewOcrPage.Text = ocrPageReview.ButtonText(revision is null ? null :
            new OcrPageReviewFlow.Source(documentId, pageIndex, detachedOcr is not null, revision));
        ReadingPage.DescribeButton(reviewOcrPage, reviewOcrPage.Text);
        reset.IsEnabled = edit.IsEnabled && lines[index].Correction is not null;
    }

    private enum LineAction { Edit, Suggest, Reset }

    private async Task ReviewOcrWordAsync()
    {
        if (!reviewOcrWord.IsEnabled || !reviewOcrWord.IsVisible || activeWord is not { } active ||
            session.Document is not { } document || document.Id != documentId ||
            CurrentWord(active.LineIndex, active.Word) != active.Selection)
            return;
        int? page = detachedOcr is null ? null : pageIndex;
        var cached = detachedOcr;
        var line = lines[active.LineIndex];
        var revision = cached?.Revision ?? document.CurrentRevisionId;
        if (revision is null) return;
        bool IsCurrent() => visible && session.Document is { } current && current.Id == documentId &&
            activeWord == active && revision == (detachedOcr?.Revision ?? current.CurrentRevisionId) &&
            (!page.HasValue || page == pageIndex);

        editing = true;
        UpdateEditButtons();
        try
        {
            var hint = await OcrPageReviewFlow.LoadAsync(session, IsCurrent, value => editStatus.Text = value,
                "Qwen перевіряє слово…",
                cachedOnly => session.ReviewOcrWordAsync(documentId, page, revision, active.Selection, cachedOnly));
            if (hint is null || !IsCurrent()) return;
            if (hint.Replacement is not { } replacement)
            {
                editStatus.Text = "Qwen не запропонував виправлення для цього слова.";
                return;
            }
            editStatus.Text = $"Можливе виправлення OCR · {active.Selection.Text} → {replacement}";
            var apply = await DisplayAlertAsync("Виправити розпізнане слово?",
                $"{active.Selection.Text} → {replacement}", "Застосувати", "Залишити");
            if (!apply || !IsCurrent() || CurrentWord(active.LineIndex, active.Word) != active.Selection ||
                line.SourceSpan is not { } lineSpan) return;
            var reading = (line.Correction ?? line.Text).Trim();
            var start = active.Selection.StartUtf16 - lineSpan.StartUtf16;
            var length = active.Selection.LengthUtf16;
            if (start < 0 || start > reading.Length - length ||
                !reading.AsSpan(start, length).SequenceEqual(active.Selection.Text))
            {
                editStatus.Text = "Позиція слова змінилася. Виберіть його знову.";
                return;
            }
            var corrected = string.Concat(reading.AsSpan(0, start), replacement, reading.AsSpan(start + length));
            if (await SaveOcrCorrectionAsync(active.LineIndex, line, cached, corrected, revision))
                editStatus.Text = "Виправлення збережено в тексті для читання.";
        }
        catch (Exception error) { if (visible && session.Document?.Id == documentId) editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            UpdateEditButtons();
        }
    }

    private async Task ReviewOcrLineAsync()
    {
        var index = LineIndex();
        if (!reviewOcrLine.IsEnabled || index < 0 || session.Document is not { } document ||
            document.Id != documentId) return;
        var cached = detachedOcr;
        var line = lines[index];
        var revision = cached?.Revision ?? document.CurrentRevisionId;
        if (revision is null) return;
        bool IsCurrent() => visible && LineIndex() == index && index < lines.Length && lines[index] == line &&
            session.Document is { } current && current.Id == documentId &&
            (detachedOcr?.Revision ?? current.CurrentRevisionId) == revision;

        editing = true;
        UpdateEditButtons();
        try
        {
            var hint = await OcrPageReviewFlow.LoadAsync(session, IsCurrent, value => editStatus.Text = value,
                "Qwen перевіряє рядок…",
                cachedOnly => session.ReviewOcrLineAsync(documentId, cached is null ? null : line.Page, revision, index, cachedOnly));
            if (hint is null || !IsCurrent()) return;
            if (hint.Replacement is not { } correction)
            {
                editStatus.Text = "Qwen не запропонував виправлень для цього рядка.";
                return;
            }
            var apply = await DisplayAlertAsync("Застосувати виправлення OCR?",
                $"{(line.Correction ?? line.Text).Trim()}\n\n{correction}", "Застосувати", "Залишити");
            if (!IsCurrent()) return;
            if (!apply)
            {
                editStatus.Text = "";
                return;
            }
            if (await SaveOcrCorrectionAsync(index, line, cached, correction, revision))
                editStatus.Text = "Виправлення збережено в тексті для читання.";
        }
        catch (Exception error) { if (IsCurrent()) editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            UpdateEditButtons();
        }
    }

    private async Task ReviewOcrPageAsync()
    {
        if (!reviewOcrPage.IsEnabled || ocrPageReview.IsRunning ||
            session.Document is not { } document || document.Id != documentId)
            return;
        var page = pageIndex;
        var detached = detachedOcr is not null;
        var revision = detachedOcr?.Revision ?? document.CurrentRevisionId;
        if (revision is null) return;
        bool IsCurrent() => visible && page == pageIndex && detached == (detachedOcr is not null) &&
            (!detached || detachedOcr?.Revision == revision);

        editing = true;
        UpdateEditButtons();
        try
        {
            var source = new OcrPageReviewFlow.Source(documentId, page, detached, revision);
            var applied = await ocrPageReview.RunAsync(session, Navigation, source, IsCurrent, value => editStatus.Text = value);
            if (applied is null) return;
            activeWord = null;
            focus = detached ? null : SentenceRange();
            ApplyOcrLines(applied.DetachedPage);
        }
        catch (Exception error) { if (visible && page == pageIndex) editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            UpdateEditButtons();
            if (visible && page == pageIndex && pageOcr?.IsCancellationRequested != false)
                QueuePageOcr(page);
        }
    }

    private async Task ProofreadPhotoAsync()
    {
        if (!proofreadPhoto.IsEnabled || session.Busy ||
            session.Document is not { CurrentRevisionId: { } revision } document || document.Id != documentId)
            return;

        editing = true;
        editStatus.Text = "Звіряємо текст із мовним пакетом…";
        UpdateEditButtons();
        try
        {
            var changed = await session.ReconcilePhotoOcrAsync(documentId, revision);
            if (!visible || session.Document?.Id != documentId) return;
            if (session.Error.Length > 0)
            {
                editStatus.Text = session.Error;
                return;
            }
            if (changed)
            {
                activeWord = null;
                focus = SentenceRange();
                ApplyOcrLines(null);
            }
            editStatus.Text = changed ? "Текст фото уточнено." : "Впевнених виправлень не знайдено.";
        }
        finally
        {
            editing = false;
            UpdateEditButtons();
        }
    }

    private async Task CorrectAsync(LineAction action)
    {
        var index = LineIndex();
        if (index < 0 || loading || editing || leavingOriginal || session.Busy || session.Document is not { } document ||
            document.Id != documentId) return;
        var line = lines[index];
        var cached = detachedOcr;
        editing = true;
        UpdateEditButtons();
        try
        {
            string? text = null;
            if (action == LineAction.Edit)
            {
                text = await DisplayPromptAsync("Виправити рядок",
                    $"Розпізнано: {line.Text}", accept: "Зберегти", cancel: "Скасувати",
                    maxLength: 4000, initialValue: line.Correction ?? line.Text);
                if (text is null) return;
                if (string.IsNullOrWhiteSpace(text))
                {
                    editStatus.Text = "Введіть текст рядка.";
                    return;
                }
            }
            if (action == LineAction.Suggest)
            {
                text = await PickSuggestionAsync(document.Language, line.Correction ?? line.Text);
                if (text is null) return;
            }
            if (await SaveOcrCorrectionAsync(index, line, cached, text, document.CurrentRevisionId))
                editStatus.Text = action == LineAction.Reset ? "Початковий текст відновлено." : "Виправлення збережено.";
        }
        catch (Exception error) { editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            UpdateEditButtons();
        }
    }

    private async Task<bool> SaveOcrCorrectionAsync(int index, OcrLine line, DetachedPdfOcrPage? cached,
        string? correction, string? revision)
    {
        if (session.Busy || session.Document is not { } document || document.Id != documentId) return false;
        if (cached is not null)
        {
            var updated = await session.CorrectDetachedPdfOcrAsync(documentId, line.Page, cached.Revision, index, correction);
            activeWord = null;
            ApplyOcrLines(updated);
            return true;
        }
        if (document.OcrLines is not { } current || index >= current.Length ||
            current[index].Page != line.Page || current[index].Text != line.Text ||
            current[index].Correction != line.Correction) return false;
        await session.CorrectOcrLineAsync(documentId, index, correction, revision);
        if (session.Error.Length != 0)
        {
            editStatus.Text = session.Error;
            return false;
        }
        activeWord = null;
        focus = SentenceRange();
        ApplyOcrLines(null);
        return true;
    }

    private async Task<string?> PickSuggestionAsync(string language, string text)
    {
        editStatus.Text = "Пошук у мовному пакеті…";
        var suggestions = await session.SuggestOcrAsync(language, text);
        if (suggestions is null)
        {
            editStatus.Text = "Встановіть пакет аналізу цієї мови в налаштуваннях.";
            return null;
        }
        if (suggestions.Length == 0)
        {
            editStatus.Text = "У мовному пакеті немає підказок для цього рядка.";
            return null;
        }
        editStatus.Text = "";
        var choices = suggestions.ToDictionary(suggestion =>
            $"{suggestion.Source} → {suggestion.Replacement} · позиція {suggestion.StartUtf16 + 1}");
        var picked = await DisplayActionSheetAsync("Підказки з мовного пакета", "Скасувати", null, choices.Keys.ToArray());
        if (picked is null || !choices.TryGetValue(picked, out var selected)) return null;

        var range = SourceRanges.Selection(text, selected.StartUtf16, selected.LengthUtf16);
        return range is not null && range.Text == selected.Source
            ? string.Concat(text.AsSpan(0, range.StartUtf16), selected.Replacement,
                text.AsSpan(range.StartUtf16 + range.LengthUtf16))
            : null;
    }

    private sealed class LineOverlay(int width, int height, OcrLine[] lines, (int Start, int End)? focus) : IDrawable
    {
        public void Draw(ICanvas canvas, RectF area)
        {
            var fitted = AspectFit(area, width, height);
            if (fitted.Width <= 0 || fitted.Height <= 0) return;
            canvas.FillColor = Color.FromArgb("#55E39137");
            var outline = Color.FromArgb("#66714D");
            var highlight = Color.FromArgb("#B7532F");
            canvas.StrokeSize = 2;
            foreach (var line in lines)
            {
                var matched = false;
                foreach (var word in line.Words ?? [])
                {
                    if (!Overlaps(word.SourceSpan, focus)) continue;
                    matched = true;
                    DrawBox(word.X, word.Y, word.Width, word.Height, true);
                }
                DrawBox(line.X, line.Y, line.Width, line.Height, !matched && Overlaps(line.SourceSpan, focus));
            }

            void DrawBox(double x, double y, double boxWidth, double boxHeight, bool selected)
            {
                var box = new RectF(fitted.X + (float)x * fitted.Width, fitted.Y + (float)y * fitted.Height,
                    (float)boxWidth * fitted.Width, (float)boxHeight * fitted.Height);
                canvas.StrokeColor = selected ? highlight : outline;
                if (selected) canvas.FillRectangle(box);
                canvas.DrawRectangle(box);
            }
        }
    }
}
