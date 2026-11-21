using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

internal sealed class PhotoPage : ContentPage
{
    internal PhotoPage(string filename, PhotoPreview preview, OcrLine[] lines)
    {
        Title = "Вихідна фотографія";
        var image = new Image
        {
            Aspect = Aspect.AspectFit,
            Source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false)),
        };
        SemanticProperties.SetDescription(image, filename);
        var overlay = new GraphicsView
        {
            Drawable = new LineOverlay(preview.Width, preview.Height, lines),
            InputTransparent = true,
            IsVisible = false,
        };
        var button = new Button { Text = "Показати області тексту", HorizontalOptions = LayoutOptions.Start };
        button.Clicked += (_, _) =>
        {
            overlay.IsVisible = !overlay.IsVisible;
            button.Text = overlay.IsVisible ? "Приховати області тексту" : "Показати області тексту";
        };
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
        var photo = new Grid { Children = { image, overlay } };
        Grid.SetRow(photo, 1);
        content.Children.Add(button);
        content.Children.Add(photo);
        Content = content;
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
