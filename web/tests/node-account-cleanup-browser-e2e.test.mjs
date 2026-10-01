import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(operation, check, description) {
  for (let attempt=0; attempt<100; attempt++) {
    const value=await operation()
    if (check(value)) return value
    await new Promise(resolve=>setTimeout(resolve,100))
  }
  throw new Error(description)
}

test('offline computer cleanup stays pending until its revoked credential reports actual completion', {timeout:120_000},async()=>{
  const directory=await mkdtemp(path.join(tmpdir(),'ternilo-node-account-cleanup-'))
  const artifacts=process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts,{recursive:true})
  let server,node,browser,page
  try {
    const origin=`http://127.0.0.1:${await freePort()}`
    server=await initializeServer({directory:path.join(directory,'server'),origin})
    const owner=server.owner.session.access_token
    const registration=await serverRequest(origin,'/admin/registration',{token:owner})
    await serverRequest(origin,'/admin/registration',{token:owner,method:'PATCH',body:{mode:'open',require_approval:false,revision:registration.revision}})
    await serverRequest(origin,'/auth/register',{body:{username:'cleanup-member',email:'cleanup-member@example.test',password:'cleanup-member-password'}})
    const member=await serverRequest(origin,'/auth/login',{body:{username:'cleanup-member',password:'cleanup-member-password'}})
    const {enrollment}=await serverRequest(origin,`/tenants/${member.personal_tenant_id}/my-computer-enrollments`,{token:member.access_token,body:{executor_id:'offline-cleanup-computer',project_id:null,ttl_seconds:600}})
    const {credential}=await serverRequest(origin,'/enrollments/consume',{body:{token:enrollment.token}})
    const localOrigin=`http://127.0.0.1:${await freePort()}`
    const nodeData=path.join(directory,'node')
    const startNode=()=>startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository,'target/debug/ternilo'),[
      'serve','--listen',new URL(localOrigin).host,'--data-dir',nodeData,
      '--node-id','offline-cleanup-computer','--gateway-url',origin.replace('http:','ws:')+'/api/v1/executors/connect','--allow-insecure-gateway',
    ],{...Object.fromEntries(Object.keys(process.env).filter(key=>key.startsWith('TERNILO_')).map(key=>[key,undefined])),TERNILO_LOCAL_TOKEN:credential.token})
    node=startNode()
    await waitForHttp(localOrigin,node)
    const memberScope={token:member.access_token,tenantId:member.personal_tenant_id}
    await until(()=>serverRequest(origin,'/execution-targets',memberScope),body=>body.executors.some(executor=>executor.connected),'Node must connect before creating account work')
    const {project}=await serverRequest(origin,'/projects',{...memberScope,body:{name:'Cleanup proof'}})
    const workspacePath=path.join(directory,'workspace')
    await mkdir(workspacePath)
    const {workspace}=await serverRequest(origin,'/workspaces',{...memberScope,body:{project_id:project.project_id,name:'Cleanup workspace',placement:'local_node',executor_id:'offline-cleanup-computer',path:workspacePath}})
    const session=await serverRequest(origin,'/sessions',{...memberScope,body:{workspace_id:workspace.workspace_id,permissions:'full_access'}})
    await serverRequest(origin,`/sessions/${session.identity.session_id}/queue`,{...memberScope,body:{content:{kind:'prompt',input:'/schedule-after 3600 account-owned-reminder'},run_id:'cleanup-schedule-owner',attachments:[],references:[],delivery:'queue'}})
    const questions=await until(()=>serverRequest(origin,`/questions?session_id=${session.identity.session_id}`,memberScope),body=>body.length>0,'the scheduled task requires approval')
    assert.equal(questions[0].question.tool_approval.tool_name,'schedule_create')
    await serverRequest(origin,`/questions/${questions[0].question.id}/answer?session_id=${session.identity.session_id}`,{...memberScope,body:{selected:['Allow once'],custom:null}})
    await until(()=>serverRequest(origin,`/sessions/${session.identity.session_id}/queue`,memberScope),body=>body.items.length===0 && !body.active_run_id,'the scheduled task must finish persisting before Node disconnects')
    const savedEvents=async()=>{
      const files=(await readdir(path.join(nodeData,'data','sessions'))).filter(file=>file.endsWith('.jsonl'))
      return (await Promise.all(files.map(file=>readFile(path.join(nodeData,'data','sessions',file),'utf8')))).flatMap(text=>text.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line)))
    }
    const originalEvents=await savedEvents()
    assert.ok(originalEvents.some(event=>event.run_id==='cleanup-schedule-owner' && event.type==='user_message' && event.provenance?.author?.user_id===member.user.user_id))
    await stopProcess(node)
    node=null
    browser=await chromium.launch({headless:true})
    page=await browser.newPage({viewport:{width:1280,height:900},serviceWorkers:'block'})
    const errors=[]
    page.on('pageerror',error=>errors.push(error.message))
    page.on('console',message=>{if(message.type()==='error')errors.push(message.text())})
    page.on('response',response=>{if(response.status()>=400)errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`)})
    await page.goto(`${origin}/admin/accounts`)
    await page.getByLabel('用户名',{exact:true}).fill(server.owner.username)
    await page.getByLabel('密码',{exact:true}).fill(server.owner.password)
    await page.getByRole('button',{name:'登录',exact:true}).click()
    const row=page.locator(`[data-admin-account="${member.user.user_id}"]`)
    await row.getByRole('button',{name:'账号“cleanup-member”的操作',exact:true}).click()
    await page.getByRole('menuitem',{name:'封禁账号',exact:true}).click()
    const dialog=page.getByRole('dialog',{name:'封禁这个账号？',exact:true})
    await dialog.getByRole('button',{name:'封禁账号',exact:true}).click()
    await row.locator('[data-account-status="banned"]').waitFor()
    await row.getByText('电脑任务清理',{exact:true}).click()
    await row.locator('[data-node-cleanup-state="pending"]').waitFor()
    const before=await serverRequest(origin,`/admin/accounts/${member.user.user_id}/node-cleanup`,{token:owner})
    assert.equal(before.length,1)
    assert.equal(before[0].request.state,'pending')
    const snapshot=await serverRequest(origin,'/executors/cleanup',{token:credential.token})
    assert.equal(snapshot.connection_allowed,false)
    assert.equal(snapshot.requests[0].request_id,before[0].request.request_id)
    const normal=await fetch(`${origin}/api/v1/executors/connect`,{headers:{authorization:`Bearer ${credential.token}`}})
    assert.equal(normal.status,401)
    const storage=JSON.parse(await readFile(path.join(nodeData,'secrets/node-authorizations.json'),'utf8'))
    await serverRequest(origin,'/executors/cleanup',{token:credential.token,body:{storage_instance_id:storage.storage_instance_id,request_id:before[0].request.request_id,status_revision:before[0].request.status_revision,state:'pending',detail:'process_state_unknown'}})
    await row.getByRole('button',{name:'刷新',exact:true}).click()
    await row.getByText('电脑重启前未记录进程退出，当前无法确认旧进程状态。请在电脑端检查。',{exact:true}).waitFor()
    node=startNode()
    await waitForHttp(localOrigin,node)
    await until(()=>serverRequest(origin,`/admin/accounts/${member.user.user_id}/node-cleanup`,{token:owner}),
      records=>records[0]?.request.state==='confirmed','Node must submit its real cleanup receipt')
    await row.getByRole('button',{name:'刷新',exact:true}).click()
    await row.locator('[data-node-cleanup-state="confirmed"]').waitFor()
    await row.locator('[data-node-cleanup-state="confirmed"]').scrollIntoViewIfNeeded()
    await page.screenshot({path:path.join(artifacts,'node-account-cleanup-confirmed-desktop.png'),animations:'disabled'})
    await page.setViewportSize({width:390,height:844})
    await row.locator('[data-node-cleanup-state="confirmed"]').scrollIntoViewIfNeeded()
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth))
    await page.screenshot({path:path.join(artifacts,'node-account-cleanup-confirmed-mobile.png'),animations:'disabled'})
    assert.deepEqual(errors,[])
    assert.ok(!node.diagnostics().includes('panicked'))
    const after=await serverRequest(origin,'/executors/cleanup',{token:credential.token})
    assert.equal(after.connection_allowed,false)
    assert.equal(after.requests[0].state,'confirmed')
    const afterEvents=await savedEvents()
    assert.ok(afterEvents.some(event=>event.type==='schedule_changed' && event.change?.operation==='delete'),'the account schedule must be durably removed')

  } catch(error) {
    if(page && !page.isClosed()) await page.screenshot({path:path.join(artifacts,'node-account-cleanup-failure.png')}).catch(()=>{})
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(node)
    await stopProcess(server)
    await rm(directory,{recursive:true,force:true})
  }
})
