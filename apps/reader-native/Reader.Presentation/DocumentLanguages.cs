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

    public static string WithPivotRoute(string label, string source, string? pivot, string target) =>
        pivot is null ? label : $"{label} · {PivotRoute(source, pivot, target)}";

    public static string Name(string code) => All.FirstOrDefault(language => language.Code == code).Name ?? code;

    public static string? TranslationTarget(string? source, string? saved, string? active, bool keepActive)
    {
        if (source is null) return null;
        bool Available(string? code) => code != source && All.Any(language => language.Code == code);
        if (keepActive && Available(active)) return active;
        var preferred = saved ?? active;
        return Available(preferred) ? preferred : source == "uk" ? "fr" : "uk";
    }
}
