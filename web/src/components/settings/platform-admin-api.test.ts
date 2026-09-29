import { afterEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import {
  createNodeLaunch,
  createOwnedNodeLaunch,
  getTenantModelUsage,
  getTenantQuota,
  listTenantAudit,
  listOwnedComputers,
  listMemberships,
  nodeLaunchCommand,
  removeMembership,
  revokeOwnedComputer,
  setMembership,
  updateTenantQuota,
} from './platform-admin-api'

afterEach(() => vi.restoreAllMocks())

describe('Control platform administration API', () => {
  it('uses tenant-scoped member routes for list, role updates, and removal', async () => {
    const request = vi.spyOn(api, 'request')
      .mockResolvedValueOnce({ memberships: [{
        user_id: 'usr-a', username: 'alice', role: 'member', created_at_ms: 1,
      }], next_cursor: null })
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(undefined)

    await expect(listMemberships('tenant / one', { query: 'Ada / 文', cursor: 'next&1' })).resolves.toMatchObject({ memberships: [{ user_id: 'usr-a' }], next_cursor: null })
    await setMembership('tenant / one', 'usr/a', 'admin')
    await removeMembership('tenant / one', 'usr/a')

    expect(request.mock.calls).toEqual([
      ['/tenants/tenant%20%2F%20one/members?limit=25&query=Ada+%2F+%E6%96%87&cursor=next%261', { signal: undefined }],
      ['/tenants/tenant%20%2F%20one/members/usr%2Fa', { method: 'PUT', body: { role: 'admin' } }],
      ['/tenants/tenant%20%2F%20one/members/usr%2Fa', { method: 'DELETE' }],
    ])
  })

  it('consumes the one-time enrollment and returns a directly runnable node command', async () => {
    const request = vi.spyOn(api, 'request')
      .mockResolvedValueOnce({ enrollment: {
        enrollment_id: 'enr-1', tenant_id: 'tenant-a', executor_id: 'home',
        expires_at_ms: 2_000, token: 'one-time-secret',
      } })
      .mockResolvedValueOnce({ credential: {
        credential_id: 'credential-1', executor_id: 'home', project_id: 'project-a',
        token: 'ternilo_node_test-secret',
      } })

    const result = await createNodeLaunch('tenant-a', {
      executorId: 'home', projectId: 'project-a', origin: 'http://control.example:8080',
    })

    expect(request.mock.calls[0]).toEqual([
      '/tenants/tenant-a/enrollments',
      { method: 'POST', body: { executor_id: 'home', project_id: 'project-a', ttl_seconds: 600 } },
    ])
    expect(request.mock.calls[1]).toEqual([
      '/enrollments/consume',
      { method: 'POST', body: { token: 'one-time-secret' } },
    ])
    expect(result.command).toContain('ternilo serve')
    expect(result.command).toContain('--token "ternilo_node_test-secret"')
    expect(result.command).toContain('--gateway-url "ws://control.example:8080/api/v1/executors/connect"' )
    expect(result.command).toContain('--allow-insecure-gateway')
    expect(result.command).not.toContain('--relay-url')
    expect(result.command).not.toContain('--allow-insecure-relay')
    expect(result.command).not.toContain('one-time-secret')
  })

  it('uses the member-owned routes and consumes the owned enrollment immediately', async () => {
    const request = vi.spyOn(api, 'request')
      .mockResolvedValueOnce({ executors: [{
        executor_id: 'member-laptop', project_id: null, state: 'active', connected: false,
        enrolled_at_ms: 1_000, last_seen_at_ms: null,
      }] })
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce({ enrollment: {
        enrollment_id: 'enr-owned', tenant_id: 'tenant / one', executor_id: 'member-laptop',
        expires_at_ms: 2_000, token: 'owned-one-time-secret',
      } })
      .mockResolvedValueOnce({ credential: {
        credential_id: 'credential-owned', executor_id: 'member-laptop', project_id: null,
        token: 'owned-node-secret',
      } })

    await expect(listOwnedComputers('tenant / one')).resolves.toHaveLength(1)
    await revokeOwnedComputer('tenant / one', 'member/laptop')
    const launch = await createOwnedNodeLaunch('tenant / one', {
      executorId: 'member-laptop', origin: 'https://control.example',
    })

    expect(request.mock.calls).toEqual([
      ['/tenants/tenant%20%2F%20one/my-computers'],
      ['/tenants/tenant%20%2F%20one/my-computers/member%2Flaptop', { method: 'DELETE' }],
      ['/tenants/tenant%20%2F%20one/my-computer-enrollments', {
        method: 'POST',
        body: { executor_id: 'member-laptop', project_id: null, ttl_seconds: 600 },
      }],
      ['/enrollments/consume', {
        method: 'POST',
        body: { token: 'owned-one-time-secret' },
      }],
    ])
    expect(launch.command).toContain('--token "owned-node-secret"' )
    expect(launch.command).toContain('--node-id="member-laptop"' )
    expect(launch.command).not.toContain('owned-one-time-secret')
  })

  it('generates a single-line command usable by Bash, PowerShell and CMD and rejects shell expansions', () => {
    const command = nodeLaunchCommand('https://control.example', 'home-node', 'ternilo_node_safe-token')
    expect(command).toBe('ternilo serve --gateway-url "wss://control.example/api/v1/executors/connect" --node-id="home-node" --token "ternilo_node_safe-token"')
    for (const unsafe of ['node";echo', '$HOME', '%PATH%', '`id`', "node's-secret", 'node\nsecret']) {
      expect(() => nodeLaunchCommand('https://control.example', unsafe, 'credential')).toThrow()
      expect(() => nodeLaunchCommand('https://control.example', 'home', unsafe)).toThrow()
    }
  })

  it('omits the plaintext-websocket opt-in for a TLS Control origin', () => {
    const command = nodeLaunchCommand('https://control.example', 'laptop', 'credential')
    expect(command).toContain("wss://control.example/api/v1/executors/connect")
    expect(command).not.toContain('--allow-insecure-gateway')
  })

  it('uses tenant-scoped quota, usage, and audit routes without inventing a client-side ledger', async () => {
    const quota = {
      max_nodes: 8,
      max_concurrent_runs: 3,
      monthly_model_tokens: 2_000_000,
      max_secrets: 24,
    }
    const request = vi.spyOn(api, 'request')
      .mockResolvedValueOnce({ quota })
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce({ period: '2026-08', totals: { requests: 1 } })
      .mockResolvedValueOnce({ entries: [{ audit_id: 'audit-1' }] })

    await expect(getTenantQuota('tenant / one')).resolves.toEqual(quota)
    await updateTenantQuota('tenant / one', quota)
    await expect(getTenantModelUsage('tenant / one', '2026-08', 125)).resolves.toMatchObject({ period: '2026-08' })
    await expect(listTenantAudit('tenant / one', 125)).resolves.toEqual([{ audit_id: 'audit-1' }])

    expect(request.mock.calls).toEqual([
      ['/tenants/tenant%20%2F%20one/quota'],
      ['/tenants/tenant%20%2F%20one/quota', { method: 'PUT', body: quota }],
      ['/tenants/tenant%20%2F%20one/model-usage?period=2026-08&limit=125'],
      ['/tenants/tenant%20%2F%20one/audit?limit=125'],
    ])
  })
})

it('validates shell arguments before issuing a one-time credential and supports Unicode Node IDs', async () => {
  const request = vi.spyOn(api, 'request')
  await expect(createOwnedNodeLaunch('tenant-a', { executorId: 'host%PATH%', origin: 'https://server.example' })).rejects.toThrow('unsupported characters')
  expect(request).not.toHaveBeenCalled()
  expect(nodeLaunchCommand('https://server.example', '-家庭电脑', 'ternilo_node_example')).toContain('--node-id="-家庭电脑"')
})
