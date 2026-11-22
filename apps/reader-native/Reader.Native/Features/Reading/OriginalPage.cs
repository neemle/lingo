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
    private readonly Button toggle = new();
    private readonly Button wordCard = new() { Text = "Картка слова" };
    private readonly Func<int, Task<PhotoPreview>> loadPage;
    private readonly Func<TextSelection, Task> showWord;
    private OcrLine[] lines;
    private readonly ReaderSession session;
    private readonly string documentId;
    private readonly Picker linePicker = new() { Title = "Розпізнані рядки" };
    private readonly Button edit = new() { Text = "Виправити" };
    private readonly Button suggest = new() { Text = "Підказки" };
    private readonly Button reset = new() { Text = "Скинути" };
    private readonly Label editStatus = new();
    private int[] currentLineIndices = [];
    private bool editing;
    private bool openingWord;
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
        TextSelection? selection, Func<TextSelection, Task> showWord)
    {
        this.loadPage = loadPage;
        this.showWord = showWord;
        this.lines = lines;
        this.documentId = documentId;
        this.session = session;
        this.pageCount = pageCount;
        focus = selection is not null ? (selection.StartUtf16, selection.StartUtf16 + selection.LengthUtf16)
            : SentenceRange();
        pageIndex = lines.FirstOrDefault(line => Overlaps(line.SourceSpan, focus))?.Page ?? initialPage;
        Title = isPdf ? "Вихідний PDF" : "Вихідна фотографія";
        SemanticProperties.SetDescription(image, filename);
        SemanticProperties.SetDescription(previous, "Попередня сторінка");
        SemanticProperties.SetDescription(next, "Наступна сторінка");
        previous.IsVisible = next.IsVisible = pageCount > 1;
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
        var controls = new HorizontalStackLayout { Spacing = 8, Children = { previous, next, toggle, wordCard, status } };
        SemanticProperties.SetDescription(linePicker, "Рядок для виправлення");
        SemanticProperties.SetDescription(reset, "Відновити початковий результат OCR для рядка");
        linePicker.SelectedIndexChanged += (_, _) => UpdateEditButtons();
        edit.Clicked += async (_, _) => await CorrectAsync(LineAction.Edit);
        suggest.Clicked += async (_, _) => await CorrectAsync(LineAction.Suggest);
        reset.Clicked += async (_, _) => await CorrectAsync(LineAction.Reset);
        var editButtons = new HorizontalStackLayout { Spacing = 8, Children = { suggest, edit, reset } };
        var editor = new Grid { ColumnSpacing = 8, ColumnDefinitions = new ColumnDefinitionCollection
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
        tap.Tapped += (_, args) => SelectWordAt(surface, args);
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
        if (image.Source is null) await LoadAsync(pageIndex);
    }

    private async Task LoadAsync(int index)
    {
        if (loading || editing || openingWord || index < 0 || index >= pageCount) return;
        loading = true;
        previous.IsEnabled = next.IsEnabled = false;
        status.Text = "Завантаження сторінки…";
        UpdateEditButtons();
        try
        {
            var preview = await loadPage(index);
            if (index != pageIndex && activeWord is not null)
            {
                activeWord = null;
                focus = SentenceRange();
            }
            pageIndex = index;
            image.Source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false));
            previewWidth = preview.Width;
            previewHeight = preview.Height;
            UpdateOverlay();
            RebuildLineChoices();
            status.Text = pageCount == 1 ? "" : $"{pageIndex + 1} / {pageCount}";
        }
        catch (Exception error) { status.Text = error.Message; }
        finally
        {
            loading = false;
            previous.IsEnabled = pageIndex > 0;
            next.IsEnabled = pageIndex + 1 < pageCount;
            UpdateEditButtons();
        }
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

    private void SelectWordAt(View surface, TappedEventArgs args)
    {
        if (loading || editing || openingWord || session.Busy || args.GetPosition(surface) is not { } point) return;
        var fitted = AspectFit(new RectF(0, 0, (float)surface.Width, (float)surface.Height), previewWidth, previewHeight);
        if (fitted.Width <= 0 || fitted.Height <= 0) return;
        var x = (point.X - fitted.X) / fitted.Width;
        var y = (point.Y - fitted.Y) / fitted.Height;
        if (!(x >= 0 && x < 1 && y >= 0 && y < 1)) return;

        for (var choice = 0; choice < currentLineIndices.Length; choice++)
        {
            var index = currentLineIndices[choice];
            foreach (var word in lines[index].Words ?? [])
            {
                if (x < word.X || x >= word.X + word.Width || y < word.Y || y >= word.Y + word.Height ||
                    CurrentWord(index, word) is not { } selected) continue;
                activeWord = (index, word, selected);
                focus = (selected.StartUtf16, selected.StartUtf16 + selected.LengthUtf16);
                overlay.IsVisible = true;
                toggle.Text = OverlayLabel();
                linePicker.SelectedIndex = choice;
                UpdateOverlay();
                UpdateEditButtons();
                return;
            }
        }
    }

    private TextSelection? CurrentWord(int index, OcrWord word)
    {
        if (session.Document is not { OcrLines: { } current } document || document.Id != documentId ||
            index < 0 || index >= current.Length || current[index].Page != pageIndex ||
            current[index].Words?.Contains(word) != true || word.SourceSpan is not { } span ||
            span.StartUtf16 < 0 || span.EndUtf16 <= span.StartUtf16 || span.EndUtf16 > document.Text.Length)
            return null;

        if (wordRanges is null || wordRanges.Text != document.Text) wordRanges = new SourceRanges(document.Text);
        var length = span.EndUtf16 - span.StartUtf16;
        var selected = wordRanges.Selection(span.StartUtf16, length);
        return selected is not null && selected.StartUtf16 == span.StartUtf16 &&
            selected.LengthUtf16 == length && selected.Text == word.Text
            ? selected : null;
    }

    private async Task OpenWordAsync()
    {
        if (loading || editing || openingWord || session.Busy || activeWord is not { } active ||
            Navigation.NavigationStack.LastOrDefault() != this) return;
        if (CurrentWord(active.LineIndex, active.Word) != active.Selection)
        {
            activeWord = null;
            focus = SentenceRange();
            UpdateOverlay();
            UpdateEditButtons();
            return;
        }

        openingWord = true;
        UpdateEditButtons();
        try
        {
            await Navigation.PopAsync();
            if (!session.Busy && CurrentWord(active.LineIndex, active.Word) == active.Selection)
                await showWord(active.Selection);
        }
        catch (Exception error) { editStatus.Text = error.Message; }
        finally
        {
            openingWord = false;
            UpdateEditButtons();
        }
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
        var canInteract = !loading && !editing && !openingWord;
        linePicker.IsEnabled = canInteract && currentLineIndices.Length > 0;
        wordCard.IsEnabled = canInteract && activeWord is not null;
        edit.IsEnabled = canInteract && index >= 0;
        suggest.IsEnabled = edit.IsEnabled;
        reset.IsEnabled = edit.IsEnabled && lines[index].Correction is not null;
    }

    private enum LineAction { Edit, Suggest, Reset }

    private async Task CorrectAsync(LineAction action)
    {
        var index = LineIndex();
        if (index < 0 || loading || editing || openingWord || session.Busy || session.Document is not { } document ||
            document.Id != documentId) return;
        var line = lines[index];
        editing = true;
        previous.IsEnabled = next.IsEnabled = false;
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
            if (session.Document?.Id != documentId || session.Document.OcrLines is not { } current ||
                index >= current.Length || current[index].Page != line.Page || current[index].Text != line.Text ||
                current[index].Correction != line.Correction) return;
            await session.CorrectOcrLineAsync(documentId, index, text);
            if (session.Error.Length != 0)
            {
                editStatus.Text = session.Error;
                return;
            }
            if (session.Document?.OcrLines is { } updated)
            {
                lines = updated;
                activeWord = null;
                focus = SentenceRange();
                UpdateOverlay();
                RebuildLineChoices();
            }
            editStatus.Text = action == LineAction.Reset ? "Початковий текст відновлено." : "Виправлення збережено.";
        }
        catch (Exception error) { editStatus.Text = error.Message; }
        finally
        {
            editing = false;
            previous.IsEnabled = pageIndex > 0;
            next.IsEnabled = pageIndex + 1 < pageCount;
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
