using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

internal sealed class OriginalPage : ContentPage
{
    private readonly Image image = new() { Aspect = Aspect.AspectFit };
    private readonly GraphicsView overlay = new() { InputTransparent = true, IsVisible = false };
    private readonly Label status = new() { VerticalOptions = LayoutOptions.Center };
    private readonly Button previous = new() { Text = "←" };
    private readonly Button next = new() { Text = "→" };
    private readonly Func<int, Task<PhotoPreview>> loadPage;
    private readonly OcrLine[] lines;
    private readonly int pageCount;
    private int pageIndex;
    private bool loading;

    internal OriginalPage(string filename, int pageCount, int initialPage, bool isPdf,
        Func<int, Task<PhotoPreview>> loadPage, OcrLine[] lines)
    {
        this.loadPage = loadPage;
        this.lines = lines;
        this.pageCount = pageCount;
        pageIndex = initialPage;
        Title = isPdf ? "Вихідний PDF" : "Вихідна фотографія";
        SemanticProperties.SetDescription(image, filename);
        SemanticProperties.SetDescription(previous, "Попередня сторінка");
        SemanticProperties.SetDescription(next, "Наступна сторінка");
        previous.IsVisible = next.IsVisible = pageCount > 1;
        previous.Clicked += async (_, _) => await LoadAsync(pageIndex - 1);
        next.Clicked += async (_, _) => await LoadAsync(pageIndex + 1);
        var toggle = new Button { Text = "Показати області тексту", IsVisible = lines.Length > 0 };
        toggle.Clicked += (_, _) =>
        {
            overlay.IsVisible = !overlay.IsVisible;
            overlay.Invalidate();
            toggle.Text = overlay.IsVisible ? "Приховати області тексту" : "Показати області тексту";
        };
        var controls = new HorizontalStackLayout { Spacing = 8, Children = { previous, next, toggle, status } };
        var content = new Grid
        {
            Padding = 16,
            RowSpacing = 8,
            RowDefinitions = new RowDefinitionCollection
            {
                new RowDefinition(GridLength.Auto),
                new RowDefinition(GridLength.Star),
            },
        };
        var surface = new Grid { Children = { image, overlay } };
        Grid.SetRow(surface, 1);
        content.Children.Add(controls);
        content.Children.Add(surface);
        Content = content;
    }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        if (image.Source is null) await LoadAsync(pageIndex);
    }

    private async Task LoadAsync(int index)
    {
        if (loading || index < 0 || index >= pageCount) return;
        loading = true;
        previous.IsEnabled = next.IsEnabled = false;
        status.Text = "Завантаження сторінки…";
        try
        {
            var preview = await loadPage(index);
            pageIndex = index;
            image.Source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false));
            overlay.Drawable = new LineOverlay(preview.Width, preview.Height,
                lines.Where(line => line.Page == pageIndex).ToArray());
            overlay.Invalidate();
            status.Text = pageCount == 1 ? "" : $"{pageIndex + 1} / {pageCount}";
        }
        catch (Exception error) { status.Text = error.Message; }
        finally
        {
            loading = false;
            previous.IsEnabled = pageIndex > 0;
            next.IsEnabled = pageIndex + 1 < pageCount;
        }
    }

    private sealed class LineOverlay(int width, int height, OcrLine[] lines) : IDrawable
    {
        public void Draw(ICanvas canvas, RectF area)
        {
            var scale = Math.Min(area.Width / width, area.Height / height);
            var contentWidth = width * scale;
            var contentHeight = height * scale;
            var left = area.X + (area.Width - contentWidth) / 2;
            var top = area.Y + (area.Height - contentHeight) / 2;
            canvas.StrokeColor = Color.FromArgb("#2669CD");
            canvas.StrokeSize = 2;
            foreach (var line in lines)
            {
                canvas.DrawRectangle(left + (float)line.X * contentWidth, top + (float)line.Y * contentHeight,
                    (float)line.Width * contentWidth, (float)line.Height * contentHeight);
            }
        }
    }
}
