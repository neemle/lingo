using System.Globalization;

namespace Reader.Native.Features.Reading;

internal static class PdfPagePrompt
{
    internal static async Task<int?> ChooseAsync(ContentPage owner, int current, int count)
    {
        var value = await owner.DisplayPromptAsync(
            "Перейти до сторінки PDF", $"Введіть номер від 1 до {count}.",
            accept: "Перейти", cancel: "Скасувати", maxLength: 5, keyboard: Keyboard.Numeric,
            initialValue: (current + 1).ToString(CultureInfo.InvariantCulture));
        if (value is null) return null;
        if (int.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out var page)
            && page >= 1 && page <= count)
            return page - 1;

        await owner.DisplayAlertAsync("Номер сторінки", $"Введіть номер від 1 до {count}.", "Гаразд");
        return null;
    }
}
