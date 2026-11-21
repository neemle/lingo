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
    Package { source: "uk", target: "fr", revision: "dea3717a7169bffc2ff692e04af8a21fd1c20cae", hashes: [
        "c87aeb75f6e42d0ee43c93d2a1e045c0a8e060f579fea216aed7232901d349e7",
        "956d1d23875ccdf6e4174a84ee87d366bc212d8c235c71bc42224863922bfa6d",
        "3116f1a7e76762bb9a7180d52b8dc34807c636aefadf79eeeef9cb1bb0678515",
        "cf758389f73cae579a8c4af90cf625bc872c38dfd9e21f8c4252574e274a5c8e",
        "6cbb453ade0328202215995f053eb078578c4df6caa69e8db543756f17374ae7",
    ] },
    Package { source: "en", target: "fr", revision: "c96fcd7f38a4c0d1ac9b61e255b47f3f7ff55e5e", hashes: [
        "df92c371911b45282caac6101fe7e7f5d5e9af7f9a99356067ffe306278ce72b",
        "173e9f493a668fe396d599e28d414a201193094e6ffd7a4678e5aab0f6d3d838",
        "78d0e717c77053f1c4b856d8661d9cb87c64f083a35418c087b9146300e4f585",
        "945c604346ce15ce4aff9001001e7f925e336d942c4087017f191871162cbdc4",
        "c32d3003ced798c78b9ca90fb69c8e23ad5b4c64c500628bdf3303bbc9272816",
    ] },
    Package { source: "fr", target: "en", revision: "c4aed37b318c763fd177aa449b44e3b783cc6c02", hashes: [
        "b3be13d046d9899d7aab8cf4ed624d9a79f5776038ba793f6b4d2ce3e02192f7",
        "78d0e717c77053f1c4b856d8661d9cb87c64f083a35418c087b9146300e4f585",
        "173e9f493a668fe396d599e28d414a201193094e6ffd7a4678e5aab0f6d3d838",
        "945c604346ce15ce4aff9001001e7f925e336d942c4087017f191871162cbdc4",
        "6e3837f34b903802c3d0d670362b997cee6e87584a1108eb3fa89e4625e4424a",
    ] },
];
