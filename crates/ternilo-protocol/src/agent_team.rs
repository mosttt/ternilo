use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use crate::{HarnessError, SubagentId};

string_id!(AgentTeamId);
string_id!(AgentTeamMemberId);
string_id!(AgentTeamTaskId);
string_id!(AgentTeamMessageId);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTeamTaskStatus {
    #[default]
    Pending,
    InProgress,
    Blocked,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTeamMemberRole {
    Lead,
    Subagent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamMember {
    pub id: AgentTeamMemberId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<AgentTeamMemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<SubagentId>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub role: AgentTeamMemberRole,
}

impl AgentTeamMember {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.id.validate()?;
        if let Some(parent) = &self.parent_id {
            parent.validate()?;
            if parent == &self.id {
                return Err(HarnessError::invalid(
                    "Agent Team member cannot be its own parent",
                ));
            }
        }
        if let Some(subagent_id) = &self.subagent_id {
            subagent_id.validate()?;
        }
        require_text(&self.label, "Agent Team member label")?;
        if let Some(provider) = &self.provider {
            require_text(provider, "Agent Team member provider")?;
        }
        match self.role {
            AgentTeamMemberRole::Lead
                if self.parent_id.is_some()
                    || self.subagent_id.is_some()
                    || self.provider.is_some() =>
            {
                Err(HarnessError::invalid(
                    "Agent Team lead cannot have subagent metadata",
                ))
            }
            AgentTeamMemberRole::Subagent
                if self.parent_id.is_none()
                    || self.subagent_id.is_none()
                    || self.provider.is_none() =>
            {
                Err(HarnessError::invalid(
                    "Agent Team subagent requires parent, subagent, and provider metadata",
                ))
            }
            AgentTeamMemberRole::Lead | AgentTeamMemberRole::Subagent => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamTask {
    pub id: AgentTeamTaskId,
    pub subject: String,
    pub description: String,
    pub status: AgentTeamTaskStatus,
    #[serde(default)]
    pub dependencies: Vec<AgentTeamTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentTeamMemberId>,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl AgentTeamTask {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.id.validate()?;
        validate_task_fields(
            &self.subject,
            &self.description,
            &self.dependencies,
            self.owner.as_ref(),
        )?;
        if self.revision == 0 || self.updated_at_ms < self.created_at_ms {
            return Err(HarnessError::invalid(
                "Agent Team task revision and timestamps are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamMessage {
    pub id: AgentTeamMessageId,
    pub from: AgentTeamMemberId,
    pub to: AgentTeamMemberId,
    pub content: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at_ms: Option<u64>,
}

impl AgentTeamMessage {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.id.validate()?;
        self.from.validate()?;
        self.to.validate()?;
        require_text(&self.content, "Agent Team message content")?;
        if self
            .read_at_ms
            .is_some_and(|read| read < self.created_at_ms)
        {
            return Err(HarnessError::invalid(
                "Agent Team message read time precedes creation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamSnapshot {
    pub team_id: AgentTeamId,
    pub current_member_id: AgentTeamMemberId,
    pub members: Vec<AgentTeamMember>,
    pub tasks: Vec<AgentTeamTask>,
    pub messages: Vec<AgentTeamMessage>,
}

impl AgentTeamSnapshot {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.team_id.validate()?;
        self.current_member_id.validate()?;
        let mut members = BTreeSet::new();
        for member in &self.members {
            member.validate()?;
            if !members.insert(member.id.clone()) {
                return Err(HarnessError::invalid(
                    "Agent Team snapshot contains duplicate members",
                ));
            }
        }
        if !members.contains(&self.current_member_id) {
            return Err(HarnessError::invalid(
                "Agent Team snapshot omits its current member",
            ));
        }
        let mut tasks = BTreeSet::new();
        for task in &self.tasks {
            task.validate()?;
            if !tasks.insert(task.id.clone()) {
                return Err(HarnessError::invalid(
                    "Agent Team snapshot contains duplicate tasks",
                ));
            }
            if task
                .owner
                .as_ref()
                .is_some_and(|owner| !members.contains(owner))
            {
                return Err(HarnessError::invalid(
                    "Agent Team task owner is not a Team member",
                ));
            }
        }
        if self.tasks.iter().any(|task| {
            task.dependencies
                .iter()
                .any(|dependency| !tasks.contains(dependency))
        }) {
            return Err(HarnessError::invalid(
                "Agent Team task dependency is not in the snapshot",
            ));
        }
        let mut messages = BTreeSet::new();
        for message in &self.messages {
            message.validate()?;
            if !messages.insert(message.id.clone())
                || !members.contains(&message.from)
                || !members.contains(&message.to)
                || (message.from != self.current_member_id && message.to != self.current_member_id)
            {
                return Err(HarnessError::invalid(
                    "Agent Team snapshot contains an invalid mailbox message",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamTaskCreate {
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: AgentTeamTaskStatus,
    #[serde(default)]
    pub dependencies: Vec<AgentTeamTaskId>,
    #[serde(default)]
    pub owner: Option<AgentTeamMemberId>,
}

impl AgentTeamTaskCreate {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_task_fields(
            &self.subject,
            &self.description,
            &self.dependencies,
            self.owner.as_ref(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamTaskReplace {
    pub expected_revision: u64,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    pub status: AgentTeamTaskStatus,
    #[serde(default)]
    pub dependencies: Vec<AgentTeamTaskId>,
    #[serde(default)]
    pub owner: Option<AgentTeamMemberId>,
}

impl AgentTeamTaskReplace {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.expected_revision == 0 {
            return Err(HarnessError::invalid(
                "Agent Team task expected revision must be positive",
            ));
        }
        validate_task_fields(
            &self.subject,
            &self.description,
            &self.dependencies,
            self.owner.as_ref(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTeamMessageSend {
    pub to: AgentTeamMemberId,
    pub content: String,
}

impl AgentTeamMessageSend {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.to.validate()?;
        require_text(&self.content, "Agent Team message content")
    }
}

fn validate_task_fields(
    subject: &str,
    _: &str,
    dependencies: &[AgentTeamTaskId],
    owner: Option<&AgentTeamMemberId>,
) -> Result<(), HarnessError> {
    require_text(subject, "Agent Team task subject")?;
    let mut unique = BTreeSet::new();
    for dependency in dependencies {
        dependency.validate()?;
        if !unique.insert(dependency) {
            return Err(HarnessError::invalid(
                "Agent Team task dependencies must be unique",
            ));
        }
    }
    if let Some(owner) = owner {
        owner.validate()?;
    }
    Ok(())
}

fn require_text(value: &str, label: &str) -> Result<(), HarnessError> {
    if value.trim().is_empty() {
        Err(HarnessError::invalid(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_task_and_message_contracts_round_trip() {
        let member = AgentTeamMemberId::new("member-lead");
        let task = AgentTeamTaskCreate {
            subject: "Review implementation".to_owned(),
            description: "Check the persistent path.".to_owned(),
            status: AgentTeamTaskStatus::InProgress,
            dependencies: vec![AgentTeamTaskId::new("task-base")],
            owner: Some(member.clone()),
        };
        task.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<AgentTeamTaskCreate>(serde_json::to_value(&task).unwrap())
                .unwrap(),
            task
        );

        let message = AgentTeamMessageSend {
            to: member,
            content: "Please report when complete.".to_owned(),
        };
        message.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<AgentTeamMessageSend>(serde_json::to_value(&message).unwrap())
                .unwrap(),
            message
        );
    }
}
