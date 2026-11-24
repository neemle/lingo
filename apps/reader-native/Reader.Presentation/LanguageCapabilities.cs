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
        TranslationPackageStatus[]? translation, TranslationRouteStatus[]? translationRoutes,
        KnowledgePackage[]? knowledge, LexicalPackage[]? lexical,
        OptionalModel[]? models, SpeechVoice[] systemVoices, string? voiceError,
        NativeRecognitionInventory recognition)
    {
        var verified = (translation ?? []).Where(package => package.HasValidatedTranslation)
            .ToLookup(package => (package.Source, package.Target));
        var routes = (translationRoutes ?? []).Select(route =>
        {
            var evidence = route.Ready && route.PivotLanguage is null
                ? verified[(route.Source, route.Target)].FirstOrDefault()?.Validation : null;
            var level = evidence is not null ? CapabilityLevel.Validated
                : route.Ready ? CapabilityLevel.Installed : CapabilityLevel.Available;
            return new CapabilityRoute(route.Source, route.Target, route.PivotLanguage, level, evidence);
        }).ToArray();

        return DocumentLanguages.All.Select(language =>
        {
            var code = language.Code;
            var ocr = RecognitionCapability(recognition, code);
            var voice = SpeechCapability(models, systemVoices, voiceError, code);
            var outgoing = routes.Where(route => route.Source == code).ToArray();
            var incoming = routes.Where(route => route.Target == code).ToArray();
            var grammar = knowledge?.FirstOrDefault(item => item.Language == code);
            var dictionary = lexical?.FirstOrDefault(item => item.Language == code);

            return new LanguageCapabilityProfile(code, language.Name, ocr, voice,
                RouteCapability(outgoing, translationRoutes is not null, outgoing: true),
                RouteCapability(incoming, translationRoutes is not null, outgoing: false),
                PackageCapability(grammar?.Name, grammar?.Installed == true, knowledge is not null,
                    grammar?.Revision, "Пакет містить словоформи, леми й граматичні ознаки."),
                PackageCapability(grammar?.Name, grammar?.SyntaxAvailable == true, knowledge is not null,
                    grammar?.Revision, grammar?.DependencyParseAvailable == true
                        ? "Доступне дерево залежностей речення та підказки для виділення."
                        : "Підказки за зв’язками між словами поблизу виділення."),
                ExpressionCapability(grammar, dictionary, knowledge is not null || lexical is not null),
                PackageCapability(dictionary?.Name, dictionary?.Installed == true, lexical is not null,
                    dictionary?.Revision, "Приклади відображаються для тих значень Wiktionary, де їх наведено."),
                outgoing, incoming);
        }).ToArray();
    }

    private static LanguageCapability RouteCapability(CapabilityRoute[] routes, bool catalogKnown, bool outgoing)
    {
        if (routes.Length == 0)
            return Unavailable(catalogKnown ? "Немає маршруту в локальному каталозі." : "Каталог перекладу недоступний.");
        var detail = string.Join(Environment.NewLine, routes.Select(route =>
        {
            var other = DocumentLanguages.Name(outgoing ? route.Target : route.Source);
            var path = route.PivotLanguage is { } pivot
                ? DocumentLanguages.PivotRoute(route.Source, pivot, route.Target)
                : $"{other} · прямий";
            var state = route.Level switch
            {
                CapabilityLevel.Validated => "пробний офлайн-переклад пройдено",
                CapabilityLevel.Installed => "файли на місці",
                _ => "потрібен пакет",
            };
            return $"{path} · {state}";
        }));
        return new LanguageCapability(routes.Max(route => route.Level), detail,
            routes.Select(route => route.PivotLanguage is null
                ? $"{route.Source}→{route.Target}" : $"{route.Source}→{route.PivotLanguage}→{route.Target}").ToArray());
    }

    private static LanguageCapability RecognitionCapability(NativeRecognitionInventory inventory, string language)
    {
        var tags = inventory.LanguageTags.Where(tag => SpeechVoice.MatchesLanguage(tag, language)).ToArray();
        var fallback = inventory.FallbackLanguages?.Contains(language) == true;
        const string portableDescription = "Tesseract · локальний OCR";
        if (tags.Length == 0)
            return fallback ? new LanguageCapability(CapabilityLevel.Installed, portableDescription, [language])
                : Unavailable(inventory.Detail);

        var detail = $"{inventory.Source} · {string.Join(", ", tags)}";
        return fallback
            ? new LanguageCapability(inventory.Level == CapabilityLevel.Validated ? CapabilityLevel.Validated : CapabilityLevel.Installed,
                $"{portableDescription}{Environment.NewLine}{detail} · резерв", tags.Append(language).Distinct().ToArray())
            : new LanguageCapability(inventory.Level, detail, tags);
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

        var descriptions = native.Select(voice => $"{voice.Name} ({voice.Language})")
            .Concat(packages.Select(ModelSpeechDescription));
        var available = packages.Any(model => FitsHardware(model.Memory));
        return new LanguageCapability(installed ? CapabilityLevel.Installed
            : available ? CapabilityLevel.Available : CapabilityLevel.Unavailable,
            string.Join(Environment.NewLine, descriptions),
            native.Select(voice => voice.Id).Concat(packages.Select(model => model.Id)).ToArray());
    }

    private static bool FitsHardware(ModelMemory? memory) =>
        memory?.TotalMib is not long total || total >= memory.MinimumTotalMib;

    private static string ModelSpeechDescription(OptionalModel model)
    {
        var memory = model.Memory;
        if (memory is not null && !FitsHardware(memory))
            return $"{model.Name} · потрібно щонайменше {memory.MinimumTotalMib} МіБ RAM";
        if (!model.FilesPresent)
            return $"{model.Name} · доступно для завантаження";
        if (memory?.Ready == true)
            return $"{model.Name} · встановлено";
        return memory?.AvailableMib is long available
            ? $"{model.Name} · встановлено, зараз доступно {available} із потрібних {memory.RequiredMib} МіБ RAM"
            : $"{model.Name} · встановлено, доступну пам’ять не вдалося визначити";
    }

    private static LanguageCapability ExpressionCapability(KnowledgePackage? grammar, LexicalPackage? lexical,
        bool catalogKnown)
    {
        if (grammar is null && lexical is null)
            return PackageCapability(null, false, catalogKnown, null, "");
        var corpus = grammar is null ? null : PackageCapability(grammar.Name, grammar.ExpressionsAvailable, true,
            grammar.Revision, grammar.ExpressionsAvailable
                ? "Вирази, зафіксовані в корпусі." : "Для корпусних виразів потрібен актуальний індекс.");
        var dictionary = lexical is null ? null : PackageCapability(lexical.Name, lexical.Installed, true,
            lexical.Revision, lexical.Installed
                ? "Словникові вирази в контексті, зокрема підтримувані змінені форми."
                : "Встановіть словниковий пакет для пошуку виразів у контексті.");
        if (corpus is null) return dictionary!;
        if (dictionary is null) return corpus;
        return new LanguageCapability((CapabilityLevel)Math.Max((int)corpus.Level, (int)dictionary.Level),
            $"{corpus.Detail}{Environment.NewLine}{dictionary.Detail}",
            corpus.Identifiers.Concat(dictionary.Identifiers).Distinct(StringComparer.Ordinal).ToArray());
    }

    private static LanguageCapability PackageCapability(string? name, bool installed, bool catalogKnown,
        string? revision, string detail) =>
        name is null
            ? Unavailable(catalogKnown ? "Пакета для цієї мови немає." : "Не вдалося прочитати каталог.")
            : new LanguageCapability(installed ? CapabilityLevel.Installed : CapabilityLevel.Available,
                $"{name}. {detail}", revision is null ? [] : [revision]);

    private static LanguageCapability Unavailable(string detail) => new(CapabilityLevel.Unavailable, detail, []);
}
