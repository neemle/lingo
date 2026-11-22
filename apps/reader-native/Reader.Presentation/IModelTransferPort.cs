using Reader.Contracts;

namespace Reader.Presentation;

public interface IModelTransferPort
{
    Task<ModelTransferPlan> ResumeModelTransferAsync(ModelTransferPackage package);
    Task<ModelTransferPlan> AcceptModelRangeAsync(
        ModelTransferPackage package, ModelTransferRange range, int status, string path);
    Task<ModelTransferPlan> AdoptModelPrefixAsync(ModelTransferPackage package, string asset, string path);
    Task<ModelTransferPlan> PauseModelTransferAsync(ModelTransferPackage package);
}
