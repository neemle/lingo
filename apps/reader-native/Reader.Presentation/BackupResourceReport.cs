using Reader.Contracts;

namespace Reader.Presentation;

public static class BackupResourceReport
{
    public static string[] Missing(BackupResource[] resources, TranslationPackageStatus[] translations,
        OptionalModel[] optional, KnowledgePackage[] grammar, LexicalPackage[] lexical)
    {
        var current = new Dictionary<(string Kind, string Id), (string Revision, bool Installed, string Name)>();
        foreach (var package in translations)
            current[("translation", $"{package.Source}-{package.Target}")] = (package.Revision, package.FilesPresent,
                $"Переклад {DocumentLanguages.Name(package.Source)} → {DocumentLanguages.Name(package.Target)}");
        foreach (var model in optional)
            current[("optional", model.Id)] = (model.Revision, model.FilesPresent, model.Name);
        foreach (var package in grammar)
            current[("grammar", package.Language)] = (package.Revision, package.Installed,
                $"Граматика: {DocumentLanguages.Name(package.Language)}");
        foreach (var package in lexical)
            current[("lexical", package.Language)] = (package.Revision, package.Installed,
                $"Словник: {DocumentLanguages.Name(package.Language)}");

        var missing = new List<string>();
        foreach (var resource in resources.Where(resource => resource is not null).Distinct())
        {
            if (!current.TryGetValue((resource.Kind, resource.Id), out var local))
                missing.Add($"{resource.Kind}: {resource.Id} ({resource.Revision}) — немає в поточному каталозі.");
            else if (local.Revision != resource.Revision)
                missing.Add($"{local.Name} — архів: {resource.Revision}; поточний каталог: {local.Revision}"
                    + (local.Installed ? "." : " (не встановлено)."));
            else if (!local.Installed)
                missing.Add($"{local.Name} ({resource.Revision}) — потрібно встановити локально.");
        }
        return missing.ToArray();
    }
}
