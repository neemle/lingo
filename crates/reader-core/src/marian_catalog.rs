use super::Package;

// Pinned Helsinki-NLP model revisions containing Hugging Face safetensors conversions.
// Hash order matches ASSETS.
pub(super) const PACKAGES: &[Package] = &[
    Package { source: "fr", target: "uk", revision: "e789f8670b3515ed604605d80d108dd97d779848", hashes: [
        "6b478f468723fe9f18339683da1ba6f7927c88e3226be03556bf516229afe0ec",
        "3116f1a7e76762bb9a7180d52b8dc34807c636aefadf79eeeef9cb1bb0678515",
        "956d1d23875ccdf6e4174a84ee87d366bc212d8c235c71bc42224863922bfa6d",
        "cf758389f73cae579a8c4af90cf625bc872c38dfd9e21f8c4252574e274a5c8e",
        "371197ce4054f0f9fa8798596623fffc7cd6eb587c2bdc9031510705714524b3",
    ] },
    Package { source: "en", target: "uk", revision: "55104bebd23cd2d87761e650aa6ac7369b379d1c", hashes: [
        "01e85031802ed6c76c43269c1dc8b641d20687b79cc700cce8cd6e0440af8854",
        "4dc9157e60a15b157c2f3d5f892379b03aed111c9fe8ac7cecba5f2aa56379ef",
        "7abd810b2df512e6fe177f8732f6a0b3107614c84cc52aba20d805b4cb0d7b6b",
        "992107700c8d5f9c1f41fefa5f72ac99050852b39d75f979c4e59344e6d14770",
        "a0f3dce3522dc5c991df5b5b669c38622234f56eeb048f9ad95e4459e0ee1088",
    ] },
];
