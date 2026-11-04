use codex_app_server_protocol::SkillMetadata;

use crate::skills_helpers::skill_description;
use crate::skills_helpers::skill_display_name;
use crate::task_mentions::MAX_TASK_TITLE_CHARS;
use crate::task_mentions::TaskMention;

use super::candidate::Candidate;
use super::candidate::MentionType;
use super::candidate::Selection;

pub(crate) fn build_search_catalog(
    skills: Option<&[SkillMetadata]>,
    tasks: &[TaskMention],
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    if let Some(skills) = skills {
        candidates.extend(skills.iter().map(skill_candidate));
    }

    candidates.extend(tasks.iter().map(|task| {
        let title = task.title.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = title.chars().take(MAX_TASK_TITLE_CHARS).collect();
        Candidate {
            display_name: title.clone(),
            description: (!task.cwd.is_empty()).then(|| task.cwd.clone()),
            search_terms: vec![
                title.clone(),
                task.cwd.chars().take(MAX_TASK_TITLE_CHARS).collect(),
                task.snippet.chars().take(MAX_TASK_TITLE_CHARS).collect(),
            ],
            mention_type: MentionType::Task,
            selection: Selection::Tool {
                insert_text: format!("@{title}"),
                path: Some(format!("thread://{}", task.thread_id)),
            },
        }
    }));

    candidates
}

fn skill_candidate(skill: &SkillMetadata) -> Candidate {
    let display_name = skill_display_name(skill);
    let description = optional_skill_description(skill);
    let skill_name = skill.name.clone();
    let search_terms = if display_name == skill.name {
        vec![skill_name.clone()]
    } else {
        vec![skill_name.clone(), display_name.clone()]
    };
    Candidate {
        display_name,
        description,
        search_terms,
        mention_type: MentionType::Skill,
        selection: Selection::Tool {
            insert_text: format!("${skill_name}"),
            path: Some(skill.path.as_str().to_owned()),
        },
    }
}

fn optional_skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill_description(skill).trim();
    (!description.is_empty()).then(|| description.to_string())
}
