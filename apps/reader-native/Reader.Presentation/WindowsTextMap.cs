using System.Text;

namespace Reader.Presentation;

public sealed class WindowsTextMap
{
    private readonly int[] offsets;
    public string Text { get; }

    public WindowsTextMap(string source)
    {
        var text = new StringBuilder();
        var map = new List<int>();
        for (var index = 0; index < source.Length; index++)
        {
            map.Add(index);
            var character = source[index];
            if (character == '\r' && index + 1 < source.Length && source[index + 1] == '\n') index++;
            text.Append(character == '\n' ? '\r' : character);
        }
        map.Add(source.Length);
        offsets = map.ToArray();
        Text = text.ToString();
    }

    public int SourceOffset(int nativeOffset) => offsets[Math.Clamp(nativeOffset, 0, offsets.Length - 1)];
    public int NativeOffset(int sourceOffset) => Array.FindLastIndex(offsets, value => value <= sourceOffset);
}
