namespace Reader.Presentation;

public sealed record TextSelection(int StartUtf16, int LengthUtf16, string Text, string? RejoinedWord = null)
{
    public string ReadingText => RejoinedWord ?? Text;
}
