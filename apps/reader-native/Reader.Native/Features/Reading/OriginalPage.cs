using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

internal sealed class OriginalPage : ContentPage
{
    private readonly Image image = new() { Aspect = Aspect.AspectFit };
    private readonly GraphicsView overlay = new() { InputTransparent = true, IsVisible = false };
    private readonly Label status = new() { VerticalOptions = LayoutOptions.Center };
    private readonly Button previous = new() { Text = "←" };
    private readonly Button next = new() { Text = "→" };
    private readonly Button jump = new() { Text = "До сторінки…" };
    private readonly Button toggle = new();
    private readonly Button readPage = new() { Text = "Читати сторінку" };
    private readonly Button wordCard = new() { Text = "Картка слова" };
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
    private readonly Picker linePicker = new() { Title = "Розпізнані рядки" };
    private readonly Button edit = new() { Text = "Виправити" };
    private readonly Button suggest = new() { Text = "Підказки" };
    private readonly Button reset = new() { Text = "Скинути" };
    private readonly Label editStatus = new();
    private int[] currentLineIndices = [];
    private bool editing;
    private bool leavingOriginal;
    private bool choosingPage;
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
        TextSelection? selection, Func<TextSelection, Task> showWord,
        Func<int, CancellationToken, Func<Task>, Task>? recognizeWindow = null,
        Func<int, TextSelection, Task>? showDetachedWord = null)
    {
        this.loadPage = loadPage;
        this.showWord = showWord;
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
        SemanticProperties.SetDescription(previous, "Попередня сторінка");
        SemanticProperties.SetDescription(next, "Наступна сторінка");
        SemanticProperties.SetDescription(jump, "Перейти за номером сторінки PDF");
        previous.IsVisible = next.IsVisible = jump.IsVisible = pageCount > 1;
        jump.Clicked += async (_, _) => await JumpPageAsync();
        previous.Clicked += async (_, _) => await LoadAsync(pageIndex - 1);
        next.Clicked += async (_, _) => await LoadAsync(pageIndex + 1);
        overlay.IsVisible = lines.Any(line => Overlaps(line.SourceSpan, focus));
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
        readPage.IsVisible = isPdf;
        readPage.Clicked += async (_, _) => await ReadPageAsync();
        var controls = new FlexLayout { Wrap = Microsoft.Maui.Layouts.FlexWrap.Wrap };
        foreach (var control in new View[] { previous, next, jump, readPage, toggle, wordCard, status })
        {
            control.Margin = new Thickness(0, 0, 8, 5);
            controls.Children.Add(control);
        }
        SemanticProperties.SetDescription(linePicker, "Рядок для виправлення");
        SemanticProperties.SetDescription(reset, "Відновити початковий результат OCR для рядка");
        linePicker.SelectedIndexChanged += (_, _) =>
        {
            var index = LineIndex();
            if (detachedOcr is not null && index >= 0 && lines[index].SourceSpan is { } span)
            {
                activeWord = null;
                focus = (span.StartUtf16, span.EndUtf16);
                UpdateOverlay();
            }
            UpdateEditButtons();
        };
        edit.Clicked += async (_, _) => await CorrectAsync(LineAction.Edit);
        suggest.Clicked += async (_, _) => await CorrectAsync(LineAction.Suggest);
        reset.Clicked += async (_, _) => await CorrectAsync(LineAction.Reset);
        var editButtons = new HorizontalStackLayout { Spacing = 8, Children = { suggest, edit, reset } };
        editor = new Grid { ColumnSpacing = 8, ColumnDefinitions = new ColumnDefinitionCollection
        {
            new ColumnDefinition(GridLength.Star), new ColumnDefinition(GridLength.Auto),
        }};
        Grid.SetColumn(editButtons, 1);
        editor.Children.Add(linePicker);
        editor.Children.Add(editButtons);

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
        var surface = new Grid { Children = { image, overlay } };
        var tap = new TapGestureRecognizer();
        tap.Tapped += (_, args) => SelectOcrAt(surface, args);
        surface.GestureRecognizers.Add(tap);
        Grid.SetRow(surface, 1);
        content.Children.Add(controls);
        content.Children.Add(surface);
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
        else QueuePageOcr(pageIndex);
    }

    protected override void OnDisappearing()
    {
        visible = false;
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
            ApplyOcrLines(cached);
            status.Text = pageCount == 1 ? "" : $"{pageIndex + 1} / {pageCount}";
            QueuePageOcr(index);
        }
        catch (Exception error) { status.Text = error.Message; }
        finally
        {
            loading = false;
            UpdateEditButtons();
        }
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

    private static RectF AspectFit(RectF area, int width, int height)
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
        if (loading || editing || leavingOriginal || session.Busy || args.GetPosition(surface) is not { } point) return;
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
                linePicker.SelectedIndex = choice;
                activeWord = (index, word, selected);
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
                linePicker.SelectedIndex = choice;
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
            return showDetachedWord is not null && length == word.Text.Length
                ? new TextSelection(span.StartUtf16, length, word.Text) : null;

        if (span.EndUtf16 > document.Text.Length) return null;
        if (wordRanges is null || wordRanges.Text != document.Text) wordRanges = new SourceRanges(document.Text);
        var selected = wordRanges.Selection(span.StartUtf16, length);
        return selected is not null && selected.StartUtf16 == span.StartUtf16 &&
            selected.LengthUtf16 == length && selected.Text == word.Text
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

    private void RebuildLineChoices()
    {
        var selected = LineIndex();
        currentLineIndices = Enumerable.Range(0, lines.Length).Where(index => lines[index].Page == pageIndex).ToArray();
        linePicker.ItemsSource = currentLineIndices.Select((index, position) =>
        {
            var line = lines[index];
            var text = new System.Globalization.StringInfo(line.Correction ?? line.Text);
            var preview = text.LengthInTextElements > 52 ? text.SubstringByTextElements(0, 52) + "…" : text.String;
            return $"{position + 1}. {preview}";
        }).ToArray();
        linePicker.SelectedIndex = currentLineIndices.Length == 0 ? -1
            : Math.Max(0, Array.IndexOf(currentLineIndices, selected));
        UpdateEditButtons();
    }

    private int LineIndex()
    {
        var selected = linePicker.SelectedIndex;
        return selected >= 0 && selected < currentLineIndices.Length ? currentLineIndices[selected] : -1;
    }

    private void UpdateEditButtons()
    {
        var index = LineIndex();
        var canInteract = !loading && !editing && !leavingOriginal && !choosingPage;
        previous.IsEnabled = canInteract && pageIndex > 0;
        next.IsEnabled = canInteract && pageIndex + 1 < pageCount;
        jump.IsEnabled = canInteract && pageCount > 1;
        linePicker.IsEnabled = canInteract && currentLineIndices.Length > 0;
        wordCard.IsEnabled = canInteract && activeWord is not null;
        readPage.IsEnabled = readPage.IsVisible && canInteract && image.Source is not null &&
            session.Document is { } document && document.Id == documentId &&
            (HasPhysicalPageAddress(document) || DocumentPages.SentencePosition(document, pageIndex).HasValue);
        edit.IsEnabled = canInteract && index >= 0;
        suggest.IsEnabled = edit.IsEnabled;
        reset.IsEnabled = edit.IsEnabled && lines[index].Correction is not null;
    }

    private enum LineAction { Edit, Suggest, Reset }

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
            if (session.Document?.Id != documentId) return;
            if (cached is not null)
            {
                var updated = await session.CorrectDetachedPdfOcrAsync(documentId, line.Page, cached.Revision, index, text);
                activeWord = null;
                ApplyOcrLines(updated);
            }
            else
            {
                if (session.Document?.OcrLines is not { } current || index >= current.Length ||
                    current[index].Page != line.Page || current[index].Text != line.Text ||
                    current[index].Correction != line.Correction) return;
                await session.CorrectOcrLineAsync(documentId, index, text);
                if (session.Error.Length != 0)
                {
                    editStatus.Text = session.Error;
                    return;
                }
                activeWord = null;
                focus = SentenceRange();
                ApplyOcrLines(null);
            }
            editStatus.Text = action == LineAction.Reset ? "Початковий текст відновлено." : "Виправлення збережено.";
        }
        catch (Exception error) { editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            UpdateEditButtons();
        }
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
