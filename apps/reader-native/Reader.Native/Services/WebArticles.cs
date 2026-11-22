using System.Net;
using System.Text;
using AngleSharp.Dom;
using AngleSharp.Html.Parser;

namespace Reader.Native.Services;

internal sealed record WebArticle(string Title, string Text, Uri Address);

internal static class WebArticles
{
    private const int MaxBytes = 4 * 1024 * 1024;
    private static readonly Lazy<HttpClient> Client = new(() => new HttpClient(new SocketsHttpHandler
    {
        AutomaticDecompression = DecompressionMethods.All,
        MaxAutomaticRedirections = 5,
        UseCookies = false,
    }));

    internal static Uri? Address(string? text) =>
        Uri.TryCreate(text?.Trim(), UriKind.Absolute, out var address) &&
        address.Scheme is "http" or "https" && address.Host.Length > 0 &&
        address.UserInfo.Length == 0 && address.AbsoluteUri.Length <= 4096 ? address : null;

    internal static async Task<WebArticle> DownloadAsync(Uri address)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        var cancel = deadline.Token;
        using var response = await Client.Value.GetAsync(address, HttpCompletionOption.ResponseHeadersRead, cancel);
        response.EnsureSuccessStatusCode();
        var destination = Address(response.RequestMessage?.RequestUri?.AbsoluteUri) ??
            throw new InvalidDataException("Сторінка перенаправила на непідтримувану адресу.");
        var type = response.Content.Headers.ContentType?.MediaType?.ToLowerInvariant();
        if (type is not (null or "text/plain" or "text/html" or "application/xhtml+xml"))
            throw new InvalidDataException("За цією адресою немає текстової сторінки.");
        await using var body = await response.Content.ReadAsStreamAsync(cancel);
        var bytes = await NativeFiles.ReadBoundedAsync(body, MaxBytes, cancel);
        using var input = new MemoryStream(bytes, writable: false);
        if (type == "text/plain")
        {
            using var reader = new StreamReader(input, Encoding.UTF8);
            return Create(destination.Host, await reader.ReadToEndAsync(cancel), destination);
        }

        var page = await new HtmlParser().ParseDocumentAsync(input, cancel);
        foreach (var element in page.QuerySelectorAll("script,style,template,noscript,nav,footer,aside,[hidden]"))
            element.Remove();
        var main = page.QuerySelector("article") ?? page.QuerySelector("main,[role=main]") ?? page.Body;
        if (main is null) throw new InvalidDataException("На сторінці немає тексту.");
        var blocks = main.QuerySelectorAll("h1,h2,h3,h4,p,li,pre")
            .Select(element => Clean(element.TextContent)).Where(text => text.Length > 0);
        var content = string.Join("\n\n", blocks);
        var title = Clean(main.QuerySelector("h1")?.TextContent ?? page.Title);
        return Create(title, content.Length > 0 ? content : Clean(main.TextContent), destination);
    }

    private static string Clean(string? text) =>
        string.Join(' ', (text ?? "").Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));

    private static WebArticle Create(string title, string text, Uri address)
    {
        if (string.IsNullOrWhiteSpace(text)) throw new InvalidDataException("На сторінці немає тексту.");
        var name = new string(title.Where(c => !char.IsControl(c) && c is not ('/' or '\\' or ':' or '*' or '?' or '"' or '<' or '>' or '|')).Take(150).ToArray()).Trim();
        return new WebArticle(name.Length == 0 ? address.Host : name, text.Trim(), address);
    }
}
