using System.Text.Json.Serialization;
using Reader.Contracts;

namespace Reader.Presentation;

[JsonConverter(typeof(JsonStringEnumConverter<CapabilityLevel>))]
public enum CapabilityLevel
{
    [JsonStringEnumMemberName("unavailable")] Unavailable,
    [JsonStringEnumMemberName("available")] Available,
    [JsonStringEnumMemberName("installed")] Installed,
    [JsonStringEnumMemberName("validated")] Validated
}

public sealed record LanguageCapability(CapabilityLevel Level, string Detail, string[] Identifiers)
{
    [JsonIgnore]
    public string Label => Level switch
    {
        CapabilityLevel.Available => "Доступно",
        CapabilityLevel.Installed => "Встановлено",
        CapabilityLevel.Validated => "Перевірено",
        _ => "—",
    };
}

public sealed record CapabilityRoute(string Source, string Target, string? PivotLanguage, CapabilityLevel Level,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] TranslationPackageValidation? Validation = null);

public sealed record LanguageCapabilityProfile(
    string Language, string Name, LanguageCapability Ocr, LanguageCapability Tts,
    LanguageCapability TranslationOut, LanguageCapability TranslationIn,
    LanguageCapability Morphology, LanguageCapability Syntax, LanguageCapability Idioms, LanguageCapability Examples,
    CapabilityRoute[] OutgoingRoutes, CapabilityRoute[] IncomingRoutes);

public sealed record NativeRecognitionInventory(CapabilityLevel Level, string[] LanguageTags, string Detail,
    string? Source = null, string[]? FallbackLanguages = null);

public sealed record LanguageCapabilityReport(int Version, string Platform, LanguageCapabilityProfile[] Languages);

public static class LanguageCapabilities
{
    public static LanguageCapabilityProfile[] Build(
        TranslationPackageStatus[]? translation, KnowledgePackage[]? knowledge, LexicalPackage[]? lexical,
        OptionalModel[]? models, SpeechVoice[] systemVoices, string? voiceError,
        NativeRecognitionInventory recognition, IReadOnlyCollection<TranslationPackageValidation>? validations = null)
    {
        var catalog = translation?.Select(item => (item.Source, item.Target)).ToHashSet() ?? [];
        var installed = translation?.Where(item => item.FilesPresent).Select(item => (item.Source, item.Target))
            .ToHashSet() ?? [];
        var verified = (translation ?? []).Where(package => package.FilesPresent)
            .SelectMany(package => (validations ?? []).Where(validation => validation.Matches(package)))
            .ToLookup(validation => (validation.Source, validation.Target));
        var routes = new List<CapabilityRoute>();
        foreach (var (from, _) in DocumentLanguages.All)
        foreach (var (to, _) in DocumentLanguages.All)
        {
            if (from == to) continue;
            var present = FindRoute(installed, from, to, CapabilityLevel.Installed);
            var route = present ?? FindRoute(catalog, from, to, CapabilityLevel.Available);
            if (route is null) continue;
            if (route.PivotLanguage is null && verified[(from, to)].FirstOrDefault() is { } evidence)
                route = route with { Level = CapabilityLevel.Validated, Validation = evidence };
            routes.Add(route);
        }

        return DocumentLanguages.All.Select(language =>
        {
            var code = language.Code;
            var ocrTags = recognition.LanguageTags.Where(tag => SpeechVoice.MatchesLanguage(tag, code)).ToArray();
            var ocr = ocrTags.Length > 0
                ? new LanguageCapability(recognition.Level, $"{recognition.Source} · {string.Join(", ", ocrTags)}", ocrTags)
                : recognition.FallbackLanguages?.Contains(code) == true
                    ? new LanguageCapability(CapabilityLevel.Installed, "Tesseract · локальний резервний OCR", [code])
                    : new LanguageCapability(CapabilityLevel.Unavailable, recognition.Detail, []);
            var voice = SpeechCapability(models, systemVoices, voiceError, code);
            var outgoing = routes.Where(route => route.Source == code).ToArray();
            var incoming = routes.Where(route => route.Target == code).ToArray();
            var grammar = knowledge?.FirstOrDefault(item => item.Language == code);
            var dictionary = lexical?.FirstOrDefault(item => item.Language == code);

            return new LanguageCapabilityProfile(code, language.Name, ocr, voice,
                RouteCapability(outgoing, translation is not null, outgoing: true),
                RouteCapability(incoming, translation is not null, outgoing: false),
                PackageCapability(grammar?.Name, grammar?.Installed == true, knowledge is not null,
                    grammar?.Revision, "Пакет містить словоформи, леми й граматичні ознаки."),
                PackageCapability(grammar?.Name, grammar?.SyntaxAvailable == true, knowledge is not null,
                    grammar?.Revision, "Підказки за спостереженими зв’язками Universal Dependencies між словами поблизу виділення."),
                PackageCapability(grammar?.Name, grammar?.ExpressionsAvailable == true, knowledge is not null,
                    grammar?.Revision, "Індекс містить сталі вирази. Стару версію пакета можна оновити."),
                PackageCapability(dictionary?.Name, dictionary?.Installed == true, lexical is not null,
                    dictionary?.Revision, "Приклади відображаються для тих значень Wiktionary, де їх наведено."),
                outgoing, incoming);
        }).ToArray();
    }

    private static CapabilityRoute? FindRoute(HashSet<(string Source, string Target)> edges,
        string source, string target, CapabilityLevel level)
    {
        if (edges.Contains((source, target))) return new CapabilityRoute(source, target, null, level);
        var pivot = DocumentLanguages.All.Select(item => item.Code)
            .Where(code => code != source && code != target)
            .OrderBy(code => code != "en")
            .ThenBy(code => code, StringComparer.Ordinal)
            .FirstOrDefault(code => edges.Contains((source, code)) && edges.Contains((code, target)));
        return pivot is null ? null : new CapabilityRoute(source, target, pivot, level);
    }

    private static LanguageCapability RouteCapability(CapabilityRoute[] routes, bool catalogKnown, bool outgoing)
    {
        if (routes.Length == 0)
            return Unavailable(catalogKnown ? "Немає маршруту в локальному каталозі." : "Каталог перекладу недоступний.");
        var detail = string.Join(Environment.NewLine, routes.Select(route =>
        {
            var other = DocumentLanguages.Name(outgoing ? route.Target : route.Source);
            var via = route.PivotLanguage is null ? "прямий" : $"через {DocumentLanguages.Name(route.PivotLanguage)}";
            var state = route.Level switch
            {
                CapabilityLevel.Validated => "пробний офлайн-переклад пройдено",
                CapabilityLevel.Installed => "файли на місці",
                _ => "потрібен пакет",
            };
            return $"{other} · {via} · {state}";
        }));
        return new LanguageCapability(routes.Max(route => route.Level), detail,
            routes.Select(route => route.PivotLanguage is null
                ? $"{route.Source}→{route.Target}" : $"{route.Source}→{route.PivotLanguage}→{route.Target}").ToArray());
    }

    private static LanguageCapability SpeechCapability(OptionalModel[]? models, SpeechVoice[] voices,
        string? voiceError, string language)
    {
        var native = SpeechVoice.ForLanguage(voices, language);
        var packages = models?.Where(item => item.Id == "supertonic-3" || (language == "uk" && item.Id == "piper-uk"))
            .ToArray() ?? [];
        var installed = native.Length > 0 || packages.Any(item => item.FilesPresent);
        if (native.Length == 0 && packages.Length == 0)
            return Unavailable(voiceError ?? (models is null
                ? "Каталог локальних голосів недоступний." : "Системних голосів і пакетів немає."));

        var descriptions = native.Select(voice => $"{voice.Name} ({voice.Language})").Concat(packages.Select(model =>
            $"{model.Name} · {(model.FilesPresent ? "файли на місці" : "доступно для завантаження")}"));
        return new LanguageCapability(installed ? CapabilityLevel.Installed : CapabilityLevel.Available,
            string.Join(Environment.NewLine, descriptions),
            native.Select(voice => voice.Id).Concat(packages.Select(model => model.Id)).ToArray());
    }

    private static LanguageCapability PackageCapability(string? name, bool installed, bool catalogKnown,
        string? revision, string detail) =>
        name is null
            ? Unavailable(catalogKnown ? "Пакета для цієї мови немає." : "Не вдалося прочитати каталог.")
            : new LanguageCapability(installed ? CapabilityLevel.Installed : CapabilityLevel.Available,
                $"{name}. {detail}", revision is null ? [] : [revision]);

    private static LanguageCapability Unavailable(string detail) => new(CapabilityLevel.Unavailable, detail, []);
}
