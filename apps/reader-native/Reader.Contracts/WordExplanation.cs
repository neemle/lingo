namespace Reader.Contracts;

public sealed record DictionaryGloss(string Text, string Language);
public sealed record WordExplanation(string Explanation, bool Cached = false, DictionaryGloss? Gloss = null);
