//! The open-weights models the desk can run and which agent uses which.
//!
//! Each file is pinned to a Hugging Face commit and a SHA-256, so every run uses the same
//! weights. The runner has 4 CPU cores, 16 GB of memory and no GPU, so one model is loaded
//! at a time.

use serde::Serialize;

/// The agents on the desk, in the order they work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    /// Writes one sector's newsbook entry for the day.
    Scout,
    /// Reads the newsbook and picks the sectors to trade.
    Strategist,
    /// Labels each headline with the kind of catalyst it is and how much buzz it carries.
    Classifier,
    /// Makes the case for owning one stock.
    Analyst,
    /// Attacks the analyst's case.
    Skeptic,
    /// Decides: keeps or sells every holding and picks every purchase.
    Manager,
}

impl Agent {
    pub const ALL: [Agent; 6] = [
        Agent::Scout,
        Agent::Strategist,
        Agent::Classifier,
        Agent::Analyst,
        Agent::Skeptic,
        Agent::Manager,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Agent::Scout => "scout",
            Agent::Strategist => "strategist",
            Agent::Classifier => "classifier",
            Agent::Analyst => "analyst",
            Agent::Skeptic => "skeptic",
            Agent::Manager => "manager",
        }
    }

    /// What the agent does, for the dashboard.
    pub fn job(self) -> &'static str {
        match self {
            Agent::Scout => "Writes each sector's newsbook entry from the day's headlines",
            Agent::Strategist => "Reads the newsbook and picks the sectors to trade",
            Agent::Classifier => "Labels every headline: what kind of catalyst, how much buzz",
            Agent::Analyst => "Makes the case for owning each stock",
            Agent::Skeptic => "Attacks the analyst's case",
            Agent::Manager => "Keeps or sells every holding and picks every purchase",
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
    /// Hidden thinking tokens the model may spend per answer: -1 unlimited, 0 none.
    pub reasoning_budget: i32,
    /// Chat-template settings sent with every request, as JSON. Empty for none.
    pub template: &'static str,
}

impl ModelSpec {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.file
        )
    }
}

pub const MODELS: [ModelSpec; 6] = [
    ModelSpec {
        id: "gpt-oss-20b",
        company: "OpenAI",
        name: "gpt-oss-20b",
        repo: "unsloth/gpt-oss-20b-GGUF",
        revision: "d449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4",
        file: "gpt-oss-20b-Q4_K_M.gguf",
        sha256: "c27536640e410032865dc68781d80a08b98f8db5e93575919af8ccc0568aeb4f",
        bytes: 11_624_759_488,
        license: "Apache-2.0",
        ctx: 12288,
        reasoning_budget: -1,
        template: r#"{"reasoning_effort":"low"}"#,
    },
    ModelSpec {
        id: "qwen3.5-9b",
        company: "Alibaba",
        name: "Qwen3.5 9B",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        bytes: 5_680_522_464,
        license: "Apache-2.0",
        ctx: 12288,
        reasoning_budget: 0,
        template: "",
    },
    ModelSpec {
        id: "gemma-4-12b",
        company: "Google",
        name: "Gemma 4 12B (QAT)",
        repo: "google/gemma-4-12B-it-qat-q4_0-gguf",
        revision: "29d097773436b69ff9feafd636ab4cf873786537",
        file: "gemma-4-12b-it-qat-q4_0.gguf",
        sha256: "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b",
        bytes: 6_975_879_296,
        license: "Apache-2.0",
        ctx: 12288,
        reasoning_budget: 0,
        template: "",
    },
    ModelSpec {
        id: "granite-4.2-8b",
        company: "IBM",
        name: "Granite 4.2 8B",
        repo: "ibm-granite/granite-4.2-8b-GGUF",
        revision: "93f3f6a8938ee922b784cf4e5b4203cd3428df8f",
        file: "granite-4.2-8b-Q4_K_M.gguf",
        sha256: "16a9369d0805f80b7377d25d87f937a90c05dc04ad79173a52001e42c9aab311",
        bytes: 5_347_917_952,
        license: "Apache-2.0",
        ctx: 12288,
        reasoning_budget: 0,
        template: "",
    },
    ModelSpec {
        id: "qwen3.5-4b",
        company: "Alibaba",
        name: "Qwen3.5 4B",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        revision: "e87f176479d0855a907a41277aca2f8ee7a09523",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_937_888,
        license: "Apache-2.0",
        ctx: 12288,
        reasoning_budget: 0,
        template: "",
    },
    // The judge of the previous desk, kept as the baseline in the model comparison.
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
        ctx: 12288,
        reasoning_budget: 0,
        template: "",
    },
];

/// The model with this id.
pub fn by_id(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Set this to a model id to run every agent on that model.
pub const MODEL_VAR: &str = "PAPER_TRADER_MODEL";

/// The model each agent runs on.
pub fn default_for(agent: Agent) -> &'static ModelSpec {
    let id = match agent {
        Agent::Scout
        | Agent::Strategist
        | Agent::Classifier
        | Agent::Analyst
        | Agent::Skeptic
        | Agent::Manager => "qwen3.5-9b",
    };
    by_id(id).expect("model registered")
}

/// The model an agent runs on in this process: [`MODEL_VAR`] when it names a model.
pub fn model_for(agent: Agent) -> &'static ModelSpec {
    std::env::var(MODEL_VAR)
        .ok()
        .and_then(|id| by_id(id.trim()))
        .unwrap_or_else(|| default_for(agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_file_is_pinned_to_a_commit_and_a_hash() {
        for m in &MODELS {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert_eq!(m.revision.len(), 40, "{}", m.id);
            assert!(m.file.to_ascii_lowercase().ends_with(".gguf"));
            assert!(m.url().contains(m.revision));
            assert!(
                m.template.is_empty()
                    || serde_json::from_str::<serde_json::Value>(m.template).is_ok()
            );
            // One model at a time must fit in the runner's 16 GB with room for the context.
            assert!(m.bytes < 12_500_000_000, "{}", m.id);
        }
    }

    #[test]
    fn ids_and_agent_names_are_unique_and_every_agent_has_a_model() {
        let ids: BTreeSet<&str> = MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), MODELS.len());
        let names: BTreeSet<&str> = Agent::ALL.iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), Agent::ALL.len());
        for agent in Agent::ALL {
            assert!(by_id(default_for(agent).id).is_some());
        }
    }
}
