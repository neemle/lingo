using Reader.Native.Services;

namespace Reader.Native.Features.Vocabulary;

public partial class VocabularyPage : ContentPage
{
    public VocabularyPage() => InitializeComponent();
    protected override async void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        await AppState.Session.RefreshAsync();
    }
}
