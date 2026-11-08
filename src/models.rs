//! The open-weights models and the job each one does.
//!
//! Every decision role runs on a model from a different company, so no single model's
//! habits or blind spots decide the whole result. Each file is pinned to an exact
//! Hugging Face commit and a SHA-256, so a run uses byte-identical weights every day.

use serde::Serialize;

/// A decision point in the desk. Each has its own model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Scores screened stocks 0 to 10.
    Triage,
    /// The tool-using research agents, and their findings summaries.
    Research,
    /// Argues for buying.
    Bull,
    /// Argues against buying.
    Bear,
    /// The portfolio manager: reasons, picks, sizes, calls the regime.
    Judge,
    /// Re-checks each holding.
    Doctor,
    /// Turns a closed trade into a lesson.
    PostMortem,
    /// Proposes strategy changes in the lab.
    Proposer,
    /// The one-prompt analyst used by the depth-1 ablation.
    SingleShot,
}

impl Role {
    pub const ALL: [Role; 9] = [
        Role::Triage,
        Role::Research,
        Role::Bull,
        Role::Bear,
        Role::Judge,
        Role::Doctor,
        Role::PostMortem,
        Role::Proposer,
        Role::SingleShot,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Role::Triage => "triage",
            Role::Research => "research",
            Role::Bull => "bull",
            Role::Bear => "bear",
            Role::Judge => "judge",
            Role::Doctor => "doctor",
            Role::PostMortem => "post_mortem",
            Role::Proposer => "proposer",
            Role::SingleShot => "single_shot",
        }
    }
}

/// One pinned model file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ModelSpec {
    pub id: &'static str,
    pub company: &'static str,
    pub name: &'static str,
    pub repo: &'static str,
    /// Hugging Face commit the file is fetched from.
    pub revision: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub license: &'static str,
    /// Context window the server is started with.
    pub ctx: u32,
}

impl ModelSpec {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.file
        )
    }
}

pub const MODELS: [ModelSpec; 9] = [
    ModelSpec {
        id: "qwen3-4b-instruct",
        company: "Alibaba",
        name: "Qwen3 4B Instruct 2507",
        repo: "unsloth/Qwen3-4B-Instruct-2507-GGUF",
        revision: "a06e946bb6b655725eafa393f4a9745d460374c9",
        file: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        sha256: "3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597",
        bytes: 2_497_281_120,
        license: "Apache-2.0",
        ctx: 16384,
    },
    ModelSpec {
        id: "gemma-4-e4b",
        company: "Google",
        name: "Gemma 4 E4B",
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        revision: "bfc15c382204943c3a8fff0c750b94ae2364d7a3",
        file: "gemma-4-E4B-it-Q4_K_M.gguf",
        sha256: "85a896a047553e842f25297ee5b031d64ff30147d9c4af17b1e4b394cd1fab87",
        bytes: 4_977_171_584,
        license: "Apache-2.0",
        ctx: 12288,
    },
    ModelSpec {
        id: "llama-3.2-3b",
        company: "Meta",
        name: "Llama 3.2 3B Instruct",
        repo: "unsloth/Llama-3.2-3B-Instruct-GGUF",
        revision: "e7d0997e49c9cb00d88b4c1a6a16aa894b0bbc31",
        file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        sha256: "6c99cc00ae910f6a532a80022cb4bc1939094527a089c29294b841c0bd87f74d",
        bytes: 2_019_377_600,
        license: "Llama 3.2 Community",
        ctx: 12288,
    },
    ModelSpec {
        id: "phi-4-mini",
        company: "Microsoft",
        name: "Phi-4 mini Instruct",
        repo: "unsloth/Phi-4-mini-instruct-GGUF",
        revision: "78eb92a46fc37e6b524df991ed9aca9bc6aa7b80",
        file: "Phi-4-mini-instruct-Q4_K_M.gguf",
        sha256: "88c00229914083cd112853aab84ed51b87bdf6b9ce42f532d8c85c7c63b1730a",
        bytes: 2_491_874_272,
        license: "MIT",
        ctx: 12288,
    },
    ModelSpec {
        id: "granite-4.0-h-micro",
        company: "IBM",
        name: "Granite 4.0 H Micro",
        repo: "ibm-granite/granite-4.0-h-micro-GGUF",
        revision: "dc1dd2585fac18a78001c677d33ef8a7bbb7eb68",
        file: "granite-4.0-h-micro-Q4_K_M.gguf",
        sha256: "bcc78b9b25450101d1ad90d4b9a264e1bac892f534dfb76066f4eec792fdf023",
        bytes: 1_942_564_512,
        license: "Apache-2.0",
        ctx: 12288,
    },
    ModelSpec {
        id: "ministral-3-3b",
        company: "Mistral AI",
        name: "Ministral 3 3B Instruct 2512",
        repo: "mistralai/Ministral-3-3B-Instruct-2512-GGUF",
        revision: "eb599d408350ea2bb60452cb86be7c7b2fc28227",
        file: "Ministral-3-3B-Instruct-2512-Q4_K_M.gguf",
        sha256: "9ed150d4367e68df0ac8e1540f6ddc65b42d0ee26378329d1ecbca60f93fc5f8",
        bytes: 2_147_023_008,
        license: "Apache-2.0",
        ctx: 12288,
    },
    ModelSpec {
        id: "smollm3-3b",
        company: "Hugging Face",
        name: "SmolLM3 3B",
        repo: "ggml-org/SmolLM3-3B-GGUF",
        revision: "4965cb60b150737b68a0408c36aeefb65078f894",
        file: "SmolLM3-Q4_K_M.gguf",
        sha256: "8334b850b7bd46238c16b0c550df2138f0889bf433809008cc17a8b05761863e",
        bytes: 1_915_305_312,
        license: "Apache-2.0",
        ctx: 12288,
    },
    ModelSpec {
        id: "nemotron-3-nano-4b",
        company: "NVIDIA",
        name: "Nemotron 3 Nano 4B",
        repo: "nvidia/NVIDIA-Nemotron-3-Nano-4B-GGUF",
        revision: "ba223d14e45525f7fae81db77ea8cabeb2fc6c25",
        file: "NVIDIA-Nemotron3-Nano-4B-Q4_K_M.gguf",
        sha256: "be5d9a656a51922f24f1f09a759cebb694e1f5d9728bf0ef9f8c972c5a0b5ef2",
        bytes: 2_837_072_864,
        license: "NVIDIA Open Model",
        ctx: 12288,
    },
    ModelSpec {
        id: "lfm2.5-2.6b",
        company: "Liquid AI",
        name: "LFM2.5 2.6B",
        repo: "LiquidAI/LFM2.5-2.6B-GGUF",
        revision: "e7caca5d835a3901a8e0d63e94009429bafafdfc",
        file: "LFM2.5-2.6B-Q4_K_M.gguf",
        sha256: "02a8b7e17487d326e46d68ce0ba24211e1b80a14c4cd0597fa73c1cd697f52ed",
        bytes: 1_674_455_040,
        license: "LFM Open",
        ctx: 12288,
    },
];

fn by_id(id: &str) -> &'static ModelSpec {
    MODELS
        .iter()
        .find(|m| m.id == id)
        .expect("model registered")
}

/// The model that makes each kind of decision.
pub fn model_for(role: Role) -> &'static ModelSpec {
    by_id(match role {
        Role::Triage => "granite-4.0-h-micro",
        Role::Research => "gemma-4-e4b",
        Role::Bull => "llama-3.2-3b",
        Role::Bear => "phi-4-mini",
        Role::Judge => "qwen3-4b-instruct",
        Role::Doctor => "ministral-3-3b",
        Role::PostMortem => "smollm3-3b",
        Role::Proposer => "nemotron-3-nano-4b",
        Role::SingleShot => "lfm2.5-2.6b",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_role_has_its_own_model_from_its_own_company() {
        let models: BTreeSet<&str> = Role::ALL.iter().map(|r| model_for(*r).id).collect();
        let companies: BTreeSet<&str> = Role::ALL.iter().map(|r| model_for(*r).company).collect();
        assert_eq!(models.len(), Role::ALL.len());
        assert_eq!(companies.len(), Role::ALL.len());
    }

    #[test]
    fn every_file_is_pinned_to_a_commit_and_a_hash() {
        for m in &MODELS {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert_eq!(m.revision.len(), 40, "{}", m.id);
            assert!(m.file.to_ascii_lowercase().ends_with(".gguf"));
            assert!(m.url().contains(m.revision));
        }
    }

    #[test]
    fn role_names_are_unique() {
        let names: BTreeSet<&str> = Role::ALL.iter().map(|r| r.name()).collect();
        assert_eq!(names.len(), Role::ALL.len());
    }
}
