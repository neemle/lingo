namespace Reader.Presentation;

public static class DocumentLanguages
{
    public static IReadOnlyList<(string Code, string Name)> All { get; } =
    [
        ("uk", "Українська"), ("en", "Англійська"), ("fr", "Французька"), ("de", "Німецька"),
        ("es", "Іспанська"), ("it", "Італійська"), ("pt", "Португальська"), ("pl", "Польська"),
        ("nl", "Нідерландська"), ("cs", "Чеська"), ("ro", "Румунська"), ("sv", "Шведська"),
    ];
}
