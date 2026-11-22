namespace Reader.Presentation;

public static class DocumentLanguages
{
    public static IReadOnlyList<(string Code, string Name)> All { get; } =
    [
        ("uk", "Українська"), ("en", "Англійська"), ("fr", "Французька"), ("de", "Німецька"),
        ("es", "Іспанська"), ("it", "Італійська"), ("pt", "Португальська"), ("pl", "Польська"),
        ("nl", "Нідерландська"), ("cs", "Чеська"), ("ro", "Румунська"), ("sv", "Шведська"),
    ];

    public static string PivotRoute(string source, string pivot, string target) =>
        $"{Name(source)} → {Name(pivot)} → {Name(target)}";

    public static string Name(string code) => All.FirstOrDefault(language => language.Code == code).Name ?? code;
}
