export type PermissionPreset = 'read_only' | 'workspace_write' | 'full_access'
export type SessionMode = 'execute' | 'plan'
export type ReasoningEffort = 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max'

export interface ResourcePermissions {
  view: boolean
  submit: boolean
  stop: boolean
  configure: boolean
}

export interface ResourceAccess {
  owner_user_id: string
  storage_user_id: string
  ownership_revision: number
  is_execution_owner: boolean
  is_owner: boolean
  can_manage_sharing?: boolean
  permissions: ResourcePermissions
  sources: ResourceAccessSource[]
  role_limited: boolean
}

export interface ResourceAccessSource {
  kind: 'owner' | 'direct_user' | 'group' | 'fork'
  resource_kind: 'project' | 'workspace' | 'session'
  resource_id: string
  resource_name?: string | null
  group_id?: string | null
  group_name?: string | null
  permissions: ResourcePermissions
}

export interface Workspace {
  access?: ResourceAccess
  workspace_id: string
  path: string
  title: string
  created_at_ms: number
  updated_at_ms: number
  placement?: 'local_node' | 'cloud'
  node_id?: string | null
  node_name?: string | null
  status?: 'online' | 'offline' | 'ready' | 'provisioning' | 'error'
  project_id?: string
  owner_user_id?: string
}

export interface SidebarOrdering {
  workspace_order: string[]
  session_order_by_account: Record<string, string[]>
}

export interface TenantSummary {
  kind: 'personal' | 'team'
  tenant_id: string
  slug: string
  display_name: string
  role: 'viewer' | 'member' | 'admin' | 'owner'
}

export interface ProjectRecord {
  tenant_id: string
  project_id: string
  name: string
  created_at_ms: number
}

export interface ExecutionTarget {
  executor_id: string
  name: string
  project_id?: string | null
  state: 'enrolled' | 'active' | 'revoked'
  connected: boolean
  last_seen_at_ms?: number | null
}

export type TenantRole = 'viewer' | 'member' | 'admin' | 'owner'

export interface MembershipRecord {
  user_id: string
  username: string
  role: TenantRole
  created_at_ms: number
}

export interface MemberPage {
  memberships: MembershipRecord[]
  next_cursor: string | null
}

export interface GroupRecord {
  group_id: string
  tenant_id: string
  name: string
  description: string | null
  member_count: number
  created_at_ms: number
  updated_at_ms: number
}

export interface GroupPage {
  groups: GroupRecord[]
  next_cursor: string | null
}

export type ShareSubject =
  | { kind: 'user'; user: { user_id: string; username: string } }
  | { kind: 'group'; group: GroupRecord }

export interface ResourceShare {
  subject: ShareSubject
  inherited: boolean
  permissions: ResourcePermissions
  created_at_ms: number
  updated_at_ms: number
}

export interface ManagedExecutionTarget extends ExecutionTarget {
  enrolled_at_ms: number
  management: ComputerManagement
}

export interface ComputerManagement {
  name: string
  notes: string
  suspended_at_ms: number | null
  removed_at_ms: number | null
  revision: number
}

export interface ComputerDetailsResponse {
  connected: boolean
  details: {
    executor: Omit<ManagedExecutionTarget, 'connected' | 'name'>
    management: ComputerManagement
    owner: { user_id: string; username: string }
    hello: { protocol_version: number; instance_nonce: string; catalog_revision: string; capabilities: string[] } | null
    workspace_count: number
    session_count: number
    credential_issued_at_ms: number | null
    credential_last_used_at_ms: number | null
  }
}

export interface TenantQuota {
  max_nodes: number
  max_concurrent_runs: number
  monthly_model_tokens: number
  max_secrets: number
}

export interface ModelUsageTotals {
  requests: number
  attempts: number
  unknown_attempts: number
  input_tokens: number
  output_tokens: number
  cached_input_tokens: number
  cache_write_tokens: number
  reasoning_tokens: number
  total_tokens: number
}

export interface ModelUsageGroup {
  provider: string
  model: string
  usage: ModelUsageTotals
}

export interface ModelUsageLedgerEntry {
  run_id: string
  actor_user_id: string
  resource_owner_user_id: string
  model_beneficiary_user_id: string
  lease_token: number
  request_id: string
  attempt: number
  provider: string
  model: string
  input_tokens: number | null
  output_tokens: number | null
  cached_input_tokens: number | null
  cache_write_tokens: number | null
  reasoning_tokens: number | null
  accounted_tokens: number | null
  provider_request_id?: string | null
  recorded_at_ms: number
  reservation_id: string
  reservation_state: string
}

export type ModelUsageAnomalyKind =
  'expired_active' | 'terminal_run_active' | 'missing_committed_tokens' | 'committed_usage_mismatch' | 'unknown_model_usage'

export interface ModelUsageReservation {
  reservation_id: string
  user_id: string
  run_id?: string | null
  reserved_tokens: number
  committed_tokens?: number | null
  state: string
  created_at_ms: number
  expires_at_ms: number
  run_state?: string | null
  ledger_tokens: number
  unknown_tokens: number
  issues: ModelUsageAnomalyKind[]
}

export interface ModelUsageReport {
  tenant_id: string
  period: string
  period_start_ms: number
  period_end_ms: number
  limit: number
  quota: {
    monthly_limit_tokens: number
    settled_tokens: number
    active_reserved_tokens: number
    unknown_reserved_tokens: number
  }
  totals: ModelUsageTotals
  groups: ModelUsageGroup[]
  ledger: ModelUsageLedgerEntry[]
  ledger_truncated: boolean
  reservations: ModelUsageReservation[]
  reservations_truncated: boolean
  anomalies: ModelUsageReservation[]
  anomalies_truncated: boolean
}

export interface AuditEntry {
  audit_id: string
  tenant_id: string
  actor_user_id?: string | null
  actor_kind: string
  action: string
  resource_type: string
  resource_id: string
  outcome: string
  metadata: unknown
  occurred_at_ms: number
  entry_hash_hex: string
}

export interface EnrollmentGrant {
  enrollment_id: string
  tenant_id: string
  executor_id: string
  name: string
  expires_at_ms: number
  token: string
}

export interface NodeCredentialGrant {
  credential_id: string
  executor_id: string
  project_id?: string | null
  token: string
}

export interface PlatformWorkspaceCreateInput {
  project_id: string
  name: string
  placement: 'local_node' | 'cloud'
  executor_id?: string
  path?: string
}

export type ModelSelection =
  | { provider: 'account_provider'; owner_user_id: string; provider_id: string; model: string; reasoning_effort?: ReasoningEffort }
  | { provider: 'profile_default' }
  | {
      provider: 'platform_model'
      grant_id: string
      model_id: string
      reasoning_effort?: ReasoningEffort
    }
  | {
      provider: 'named_provider'
      provider_id: string
      model: string
      reasoning_effort?: ReasoningEffort
    }
  | {
      provider: 'open_ai_compatible'
      base_url: string
      model: string
      api_key_env?: string | null
      timeout_ms: number
      max_attempts: number
      retry_base_delay_ms: number
    }

export interface PluginEntry {
  id: string
  kind: string
  enabled: boolean
  config: unknown
}

export interface Profile {
  plugins: PluginEntry[]
}

export interface SessionIdentity {
  tenant_id: string
  user_id: string
  agent_id: string
  session_id: string
}

export interface LocalSession {
  access?: ResourceAccess
  identity: SessionIdentity
  workspace_id: string
  placement?: 'local_node' | 'cloud'
  workspace_path: string
  parent_session_id?: string | null
  subagent?: {
    subagent_id: string
    provider: string
    transcript_kind: 'conversation' | 'process_lifecycle'
  } | null
  title: string
  permissions: PermissionPreset
  model: ModelSelection
  model_token_limit?: number | null
  agent_preset: string
  preset_plugins: PluginEntry[]
  profile_plugins: PluginEntry[]
  mode: SessionMode
  blank?: boolean
  archived_at_ms?: number | null
  created_at_ms: number
  updated_at_ms: number
}

export type AgentTeamTaskStatus = 'pending' | 'in_progress' | 'blocked' | 'completed' | 'cancelled'

export interface AgentTeamMember {
  id: string
  parent_id?: string | null
  subagent_id?: string | null
  label: string
  provider?: string | null
  role: 'lead' | 'subagent'
}

export interface AgentTeamTask {
  id: string
  subject: string
  description: string
  status: AgentTeamTaskStatus
  dependencies: string[]
  owner?: string | null
  revision: number
  created_at_ms: number
  updated_at_ms: number
}

export interface AgentTeamMessage {
  id: string
  from: string
  to: string
  content: string
  created_at_ms: number
  read_at_ms?: number | null
}

export interface AgentTeamSnapshot {
  team_id: string
  current_member_id: string
  members: AgentTeamMember[]
  tasks: AgentTeamTask[]
  messages: AgentTeamMessage[]
}

export interface AgentTeamTaskDraft {
  subject: string
  description: string
  status: AgentTeamTaskStatus
  dependencies: string[]
  owner: string | null
}

export interface ApplicationState {
  workspaces: Workspace[]
  sessions: LocalSession[]
}

export interface PluginCatalogEntry {
  kind: string
  description: string
  requires: string[]
  provides: string[]
  config_schema: Record<string, unknown>
}

export interface ApplicationCatalog {
  revision: string
  plugin_kinds: string[]
  plugins: PluginCatalogEntry[]
  host_limits?: { max_steps: number; max_tool_calls: number }
}

export interface AgentPresetSummary {
  id: string
  display_name: string
  description: string
  trust: 'system' | 'user'
}

export interface AgentPresetRoster {
  presets: AgentPresetSummary[]
  default_id: string
  authorable: boolean
}

export interface AgentPresetDocument extends AgentPresetSummary {
  profile: Profile
  base_profile?: Profile
}

export interface ProviderModelReasoning {
  default_effort: ReasoningEffort
  efforts: Partial<Record<ReasoningEffort, string | null>>
}

export interface ProviderModelDefaults {
  context_window: number
  max_output_tokens: number
  reasoning?: ProviderModelReasoning | null
}

export type ProviderReasoningSetting = { mode: 'disabled' } | { mode: 'enabled'; configuration: ProviderModelReasoning }

export interface ProviderModelValues {
  context_window?: number | null
  max_output_tokens?: number | null
  reasoning?: ProviderReasoningSetting | null
}

export type ProviderModelSettings = { mode: 'inherit' } | ({ mode: 'override' } & ProviderModelDefaults)
  | { mode: 'automatic'; upstream: ProviderModelValues; overrides: ProviderModelValues }

export interface ProviderModel {
  id: string
  display_name?: string | null
  settings: ProviderModelSettings
}

export type ProviderProtocol = 'openai-chat-completions' | 'openai-responses' | 'deepseek-responses' | 'google-gemini' | 'anthropic-messages'

export interface ProviderModelDiscoveryRequest {
  provider_id?: string
  base_url?: string
  protocol?: ProviderProtocol
  timeout_ms?: number
  api_key?: string | null
}

export interface ProviderProfile {
  id: string
  source?: 'operator' | 'user'
  display_name: string
  base_url: string
  protocol: ProviderProtocol
  api_key_ref?: string | null
  defaults: ProviderModelDefaults
  models: ProviderModel[]
  timeout_ms: number
  max_attempts: number
  retry_base_delay_ms: number
}

export interface SkillSummary {
  name: string
  description: string
  when_to_use?: string | null
  invocation: {
    model_invocable: boolean
    user_invocable: boolean
  }
  source: string
  provider: string
}

export interface SkillCatalogSnapshot {
  revision: number
  complete: boolean
  skills: SkillSummary[]
}

export interface CommandInputDescriptor {
  hint: string
  images: boolean
}

export interface CommandDescriptor {
  name: string
  description: string
  input?: CommandInputDescriptor | null
}

export interface SessionCommandCatalog {
  session_id: string
  commands: CommandDescriptor[]
}

export interface Attachment {
  name: string
  media_type: string
  content: string
}

export type SubmissionReference =
  | { kind: 'file'; path: string; file_kind: 'file' | 'directory' }
  | { kind: 'session'; session_id: string; label: string }

export interface ReferenceContextCompleteness {
  retained_items: number
  omitted_items: number
  truncated: boolean
}

export type ReferenceCandidate =
  | {
      kind: 'file'
      path: string
      file_kind: 'file' | 'directory'
      label: string
    }
  | {
      kind: 'session'
      session_id: string
      label: string
      workspace: string
      same_workspace: boolean
      updated_at_ms: number
    }

export interface ReferenceCandidateSnapshot {
  directory: string
  candidates: ReferenceCandidate[]
}

export type SubmissionDelivery = 'queue' | 'steer'
export type SubmissionPlacement = 'queued' | 'steering' | 'running'
export type SubmissionContent = { kind: 'prompt'; input: string } | { kind: 'skill'; name: string; input: string }
  | { kind: 'regenerate'; target_seq: number; input: string; skill_name?: string | null }

export type InputAuthor =
  | { kind: 'account'; user_id: string; username: string }
  | { kind: 'local' }
  | { kind: 'automation'; source: 'schedule' | 'subagent' }

export interface InputProvenance {
  input_id: string
  run_id?: string | null
  author: InputAuthor
}

export interface SessionSubmission {
  provenance?: InputProvenance | null
  id: string
  run_id: string
  content: SubmissionContent
  references: SubmissionReference[]
  attachments: Attachment[]
  placement: SubmissionPlacement
  created_at_ms: number
  updated_at_ms: number
}

/** One browser-memory user message shown until its exact durable occurrence is visible. */
export interface PendingSubmissionEcho {
  author?: InputAuthor
  request_id: string
  session_id: string
  run_id: string
  submission_id?: string
  delivery: SubmissionDelivery
  input: string
  references: SubmissionReference[]
  attachments: Attachment[]
  created_at_ms: number
}

export interface SessionInboxSnapshot {
  session_id: string
  active_run_id?: string | null
  paused: boolean
  error?: string | null
  items: SessionSubmission[]
}

export interface ToolCall {
  id: string
  name: string
  arguments: unknown
  presentation?: ToolPresentationDescriptor | null
}

export type ToolPresentationIconKind =
  'wrench' | 'puzzle' | 'sparkles' | 'file' | 'terminal' | 'search' | 'globe' | 'database' | 'code' | 'unknown'

export interface ToolPresentationField {
  label: string
  path: string[]
}

export type ToolPresentationResultKind = 'text' | 'markdown' | 'json' | 'table' | 'unknown'

export interface ToolPresentationDescriptor {
  title: string
  icon_kind: ToolPresentationIconKind
  input_summary?: ToolPresentationField[]
  result: {
    kind: ToolPresentationResultKind
    columns?: ToolPresentationField[]
  }
}

export interface ToolOutput {
  content: string
  is_error: boolean
}

export interface ModelResponse {
  provider: string
  model: string
  content: string
  reasoning_content?: string | null
  tool_calls?: ToolCall[]
  usage?: {
    input_tokens: number
    output_tokens: number
    cached_input_tokens?: number
    cache_write_tokens?: number
    reasoning_tokens?: number
  } | null
  finish_reason: 'stop' | 'tool_calls' | 'max_tokens'
  provider_request_id?: string | null
  attempts?: number
  request_digest?: string | null
  replayed?: boolean
}

export interface SessionEvent extends Record<string, unknown> {
  provenance?: InputProvenance | null
  seq: number
  occurred_at_ms: number
  run_id: string
  type: string
  step?: number
  system_prompt?: string
  delta?: string
  content?: string
  display_content?: string | null
  attachments?: Attachment[]
  references?: SubmissionReference[]
  reference?: SubmissionReference | null
  completeness?: ReferenceContextCompleteness | null
  source?:
    | { kind: 'schedule'; schedule_id: string; created_seq: number; dispatched_seq: number }
    | { kind: 'skill_invocation'; name: string }
    | {
        kind: 'submission'
        submission_id: string
        created_at_ms: number
        delivery: SubmissionDelivery
        skill_name?: string | null
        regenerate_from?: number | null
      }
    | null
  response?: ModelResponse
  call?: ToolCall
  call_id?: string
  parent_call_id?: string
  name?: string
  output?: ToolOutput
  retained_output?: Attachment | null
  job?: {
    job_id: string
    command: string
    status: 'running' | 'completed' | 'failed' | 'cancelled'
    result?: { exit_code?: number | null; timed_out?: boolean } | null
    error?: string | null
  }
  message?: string
  title?: string
  retry_id?: string
  retry?: number
  max_retries?: number | null
  delay_ms?: number
  failure?: { message: string; code?: string | null }
  /** Optional terminal cause used by the Chat max-token notice. */
  finish_reason?: 'completed' | 'max_tokens'
}

export interface SessionCommandReceipt {
  command_id: string
  events: SessionEvent[]
}

export interface SessionStats {
  events: number
  turns: number
  completed_turns: number
  failed_turns: number
  cancelled_turns: number
  steps: number
  tool_calls: number
  user_messages: number
  assistant_messages: number
  estimated_logged_tokens: number
  exact_input_tokens: number
  exact_output_tokens: number
  exact_reasoning_tokens: number
  cached_input_tokens: number
  model_attempts: number
  measured_model_responses: number
  model_duration_ms: number
  tool_duration_ms: number
  first_token_duration_ms: number
  measured_first_tokens: number
  generation_duration_ms: number
  generation_output_tokens: number
}

export interface SessionProjection {
  session_id: string
  as_of_seq: number | null
  values: Record<string, unknown>
}

export interface UserQuestionOption {
  label: string
  description?: string | null
}

export interface UserQuestion {
  id: string
  question: string
  detail?: string | null
  header?: string | null
  options: UserQuestionOption[]
  multi_select: boolean
  presentation?: {
    kind: 'plan_review'
    title: string
    plan: string
    approve_label: string
  } | null
  tool_approval?: {
    tool_name: string
    call_id: string
    reason: string
    arguments: unknown
    presentation?: ToolPresentationDescriptor | null
  } | null
}

export interface UserQuestionAnswer {
  selected: string[]
  custom?: string | null
}

export interface PendingQuestion {
  session_id: string
  question: UserQuestion
}

export interface DirectoryEntry {
  name: string
  path: string
  hidden: boolean
}

export interface DirectoryListing {
  path: string
  home: string
  crumbs: DirectoryEntry[]
  entries: DirectoryEntry[]
  truncated: boolean
}

export interface SessionSearchHit {
  session_id: string
  workspace_id: string
  title: string
  updated_at_ms: number
  event_seq: number | null
  occurred_at_ms: number | null
  run_id: string | null
  category: string | null
  excerpt: string
}

export interface CredentialReferenceInfo {
  reference: string
  configured: boolean
  source?: 'environment' | 'managed' | null
  writable: boolean
}

export interface CredentialRecordInfo {
  key: string
  kind: string
  updated_at_ms: number
}

export interface CredentialInventory {
  references: CredentialReferenceInfo[]
  records: CredentialRecordInfo[]
}

export interface RhaiExtensionLimits {
  max_operations: number
  max_wall_ms: number
  max_input_bytes: number
  max_output_bytes: number
  max_string_bytes: number
  max_collection_items: number
  max_call_levels: number
  max_expr_depth: number
  max_variables: number
  max_functions: number
  max_workspace_read_bytes: number
}

export interface WasmComponentExtensionLimits {
  fuel: number
  max_memory_bytes: number
  max_input_bytes: number
  max_output_bytes: number
  max_workspace_read_bytes: number
}

export type ExtensionRuntime =
  | { kind: 'rhai'; limits: RhaiExtensionLimits }
  | {
      kind: 'wasm-component'
      world: string
      limits: WasmComponentExtensionLimits
    }

export type ExtensionPayload = { kind: 'utf8'; content: string } | { kind: 'base64'; content: string }

export interface ExtensionToolContribution {
  handler: string
  spec: {
    name: string
    description: string
    input_schema: Record<string, unknown>
  }
  output_schema: Record<string, unknown>
  effect: string
  presentation?: ToolPresentationDescriptor | null
}

export interface ExtensionPromptSectionContribution {
  id: string
  order: number
  content: string
}

export interface SkillInvocationPolicy {
  model_invocable: boolean
  user_invocable: boolean
}

export interface ExtensionSkillContribution {
  name: string
  description: string
  when_to_use?: string | null
  invocation?: SkillInvocationPolicy
  content: string
}

export type ExtensionHookPoint =
  | 'session_start'
  | 'user_prompt_submit'
  | 'pre_tool_use'
  | 'post_tool_use'
  | 'stop'

export type ExtensionHookMatcher =
  | { kind: 'all' }
  | { kind: 'tool_names'; names: string[] }

export interface ExtensionHookContribution {
  id: string
  point: ExtensionHookPoint
  handler: string
  matcher: ExtensionHookMatcher
}

export interface ExtensionCommandInput {
  hint: string
  field: string
  images?: boolean
}

export interface ExtensionCommandContribution {
  name: string
  description: string
  tool: string
  input?: ExtensionCommandInput | null
  fixed_arguments: Record<string, unknown>
}

export interface ExtensionProviderCredential {
  required: boolean
  suggested_ref?: string | null
}

export interface ExtensionProviderContribution {
  id: string
  display_name: string
  base_url: string
  protocol: ProviderProtocol
  defaults: ProviderModelDefaults
  models: ProviderModel[]
  timeout_ms: number
  max_attempts: number
  retry_base_delay_ms: number
  credential: ExtensionProviderCredential
}

export interface ExtensionPackageManifest {
  schema_version: 1
  package_id: string
  version: string
  description?: string
  source: string
  publisher_key_id: string
  payload_sha256: string
  runtime: ExtensionRuntime
  config_schema: Record<string, unknown>
  contributions: {
    tools: ExtensionToolContribution[]
    prompt_sections: ExtensionPromptSectionContribution[]
    skills: ExtensionSkillContribution[]
    hooks: ExtensionHookContribution[]
    commands: ExtensionCommandContribution[]
    providers: ExtensionProviderContribution[]
  }
  requested_capabilities: string[]
}

export interface SignedExtensionBundle {
  manifest: ExtensionPackageManifest
  payload: ExtensionPayload
  signature_base64: string
}

export interface InstalledExtension {
  manifest: ExtensionPackageManifest
  granted_capabilities: string[]
  enabled: boolean
  revoked: boolean
  installed_at_ms: number
  updated_at_ms: number
}

export interface ExtensionInventory {
  publishers: Array<{
    trust: {
      key_id: string
      allowed_sources: string[]
      public_key_base64?: string
    }
    revoked: boolean
    added_at_ms: number
    updated_at_ms: number
  }>
  extensions: InstalledExtension[]
}

export type SettingsSection =
  'general' | 'models' | 'plugins' | 'presets' | 'credentials' | 'computers' | 'platform' | 'instance' | 'about'

export interface ToastMessage {
  id: string
  message: string
  kind: 'success' | 'error' | 'info'
}

export interface SessionServiceSnapshot {
  id: string
  name: string
  kind: 'mcp' | 'lsp'
  status: 'idle' | 'starting' | 'running' | 'stopping' | 'stopped' | 'failed'
  active_calls: number
  error?: string | null
}

export interface SessionEventPage {
  events: SessionEvent[]
  next_before_seq: number | null
}
