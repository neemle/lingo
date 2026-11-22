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
    private readonly Func<int, Task<PhotoPreview>> loadPage;
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
    private readonly int pageCount;
    private int pageIndex;
    private bool loading;
    private (int Start, int End)? focus;
    private int previewWidth;
    private int previewHeight;

    internal OriginalPage(string filename, int pageCount, int initialPage, bool isPdf,
        Func<int, Task<PhotoPreview>> loadPage, string documentId, ReaderSession session, OcrLine[] lines,
        TextSelection? selection)
    {
        this.loadPage = loadPage;
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
        var toggle = new Button { Text = OverlayLabel(), IsVisible = lines.Length > 0 };
        toggle.Clicked += (_, _) =>
        {
            overlay.IsVisible = !overlay.IsVisible;
            overlay.Invalidate();
            toggle.Text = OverlayLabel();
        };
        var controls = new HorizontalStackLayout { Spacing = 8, Children = { previous, next, toggle, status } };
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
        if (loading || editing || index < 0 || index >= pageCount) return;
        loading = true;
        previous.IsEnabled = next.IsEnabled = false;
        status.Text = "Завантаження сторінки…";
        UpdateEditButtons();
        try
        {
            var preview = await loadPage(index);
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
        linePicker.IsEnabled = !loading && !editing && currentLineIndices.Length > 0;
        edit.IsEnabled = !loading && !editing && index >= 0;
        suggest.IsEnabled = edit.IsEnabled;
        reset.IsEnabled = edit.IsEnabled && lines[index].Correction is not null;
    }

    private enum LineAction { Edit, Suggest, Reset }

    private async Task CorrectAsync(LineAction action)
    {
        var index = LineIndex();
        if (index < 0 || loading || editing || session.Busy || session.Document is not { } document ||
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
            if (width <= 0 || height <= 0) return;
            var scale = Math.Min(area.Width / width, area.Height / height);
            var contentWidth = width * scale;
            var contentHeight = height * scale;
            var left = area.X + (area.Width - contentWidth) / 2;
            var top = area.Y + (area.Height - contentHeight) / 2;
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
                var box = new RectF(left + (float)x * contentWidth, top + (float)y * contentHeight,
                    (float)boxWidth * contentWidth, (float)boxHeight * contentHeight);
                canvas.StrokeColor = selected ? highlight : outline;
                if (selected) canvas.FillRectangle(box);
                canvas.DrawRectangle(box);
            }
        }
    }
}
