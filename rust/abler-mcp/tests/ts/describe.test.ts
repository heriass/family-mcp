import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'bun:test';
import { Client } from '@modelcontextprotocol/client';
import { importCookies, loadSession, saveSession } from '../../../../packages/abler-mcp/src/auth.js';
import { useScratchStore } from '../../../../packages/abler-mcp/test/scratch.js';
import { serveStdio } from './rust-abler.js';

useScratchStore();
const named=(kind:string,name:string)=>({kind,name,ofType:null});
const scalar=(name:string)=>named('SCALAR',name);
const wrap=(kind:string,ofType:object)=>({kind,name:null,ofType});
const field=(name:string,type:object,args:object[]=[],description:string|null=null)=>({name,type,args,description});
const schema={
  queryType:{name:'RootQuery'},mutationType:{name:'RootMutation'},
  types:[
    {kind:'OBJECT',name:'RootQuery',fields:[field('lookup',wrap('NON_NULL',named('OBJECT','Profile')),[{name:'ids',type:wrap('LIST',wrap('NON_NULL',scalar('ID')))}],'Lookup documentation'),field('profile',named('OBJECT','Profile'),[],'private-access documentation')]},
    {kind:'OBJECT',name:'RootMutation',fields:[field('markConversationRead',wrap('NON_NULL',scalar('Boolean')),[{name:'input',type:wrap('NON_NULL',named('INPUT_OBJECT','ReadInput'))},{name:'mode',type:named('ENUM','ReadMode')}],'Mark schema metadata'),field('sendMessage',scalar('Boolean'),[{name:'input',type:named('INPUT_OBJECT','SendInput')}])]},
    {kind:'INPUT_OBJECT',name:'ReadInput',inputFields:[{name:'ids',type:wrap('LIST',wrap('NON_NULL',scalar('ID')))},{name:'related',type:named('INPUT_OBJECT','ReadInput')}]},
    {kind:'INPUT_OBJECT',name:'SendInput',inputFields:[{name:'attachment',type:named('INPUT_OBJECT','AttachmentInput')}]},
    {kind:'INPUT_OBJECT',name:'AttachmentInput',inputFields:[{name:'priority',type:named('ENUM','Priority')}]},
    {kind:'ENUM',name:'ReadMode',enumValues:[{name:'READ'},{name:'UNREAD'}]},
    {kind:'ENUM',name:'Priority',enumValues:[{name:'NORMAL'},{name:'HIGH'}]},
    {kind:'OBJECT',name:'Profile',fields:[field('privatePersonRecord',scalar('String'),[],'Personal records must never be returned')]},
  ],
};

test('describe_api returns only typed schema metadata, filters fields and uses existing refresh/rotation',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'abler-describe-'));
  const path=join(directory,'session.json');
  const cookie={domain:'www.abler.io',path:'/',expires:Date.now()/1000+3600};
  await saveSession(path,await importCookies([{...cookie,name:'refreshToken',value:'private-refresh'},{...cookie,name:'id_token',value:'private-access'}]));
  let calls=0;let refreshes=0;let rejectOnce=false;let large=false;
  const served=await serveStdio(path,(url,init)=>{
    assert.equal(init.method,'POST');
    if(url.endsWith('/oauth/token')){
      refreshes++;return Response.json({access_token:'private-renewed'},{headers:{'Set-Cookie':'id_token=private-renewed; Path=/; Secure; HttpOnly; Max-Age=3600'}});
    }
    assert.ok(url.endsWith('/graphql'));
    const body=JSON.parse(String(init.body));calls++;
    assert.equal(body.operationName,'DescribeApi');
    assert.match(body.query,/query DescribeApi/);assert.match(body.query,/__schema/);
    assert.ok(!body.query.includes('privatePersonRecord'));
    assert.ok(!body.query.includes('defaultValue'));
    if(rejectOnce){rejectOnce=false;return Response.json({errors:[{message:'private-access',extensions:{code:'UNAUTHENTICATED'}}]});}
    const model=structuredClone(schema);
    model.types[0]!.fields![1]!.description = (new Headers(init.headers).get('cookie')?.match(/id_token=([^;]+)/)?.[1] ?? '') + ' documentation';
    if(large) model.types[0]!.fields=Array.from({length:100},(_,i)=>field(`field${i}`,scalar('String'),[],'x'.repeat(200)));
    return Response.json({data:{__schema:model,me:{displayName:'Private actual person'}}},{headers:{'Set-Cookie':'id_token=private-rotated; Path=/; Secure; HttpOnly; Max-Age=3600'}});
  });
  const client=new Client({name:'describe-test',version:'1.0.0'});await client.connect(served.transport);
  try{
    const tools=(await client.listTools()).tools;
    assert.equal(tools.length,9);assert.equal(tools.find(t=>t.name==='describe_api')?.annotations?.readOnlyHint,true);
    const call=(args:Record<string,unknown>={})=>client.callTool({name:'describe_api',arguments:args});
    const first=await call();assert.ok(!first.isError,JSON.stringify(first));
    const out=first.structuredContent as {introspection:boolean,query:{name:string,arguments:{type:string}[],type:string,returnType:string}[],mutation:object[],inputObjects:Record<string,object[]>,enums:Record<string,string[]>};
    assert.equal(out.introspection,true);assert.equal(out.query[0]?.arguments[0]?.type,'[ID!]');
    assert.equal(out.query[0]?.type,'Profile!');assert.equal(out.query[0]?.returnType,'Profile');
    assert.deepEqual(out.enums.ReadMode,['READ','UNREAD']);assert.deepEqual(out.enums.Priority,['NORMAL','HIGH']);
    assert.ok(out.inputObjects.ReadInput);assert.ok(out.inputObjects.SendInput);assert.ok(out.inputObjects.AttachmentInput);
    assert.ok(!JSON.stringify(first).includes('private-access'));assert.ok(!JSON.stringify(first).includes('private-refresh'));
    assert.ok(!JSON.stringify(first).includes('Private actual person'));assert.ok(!JSON.stringify(first).includes('privatePersonRecord'));
    const filtered=await call({filter:'mARK'});assert.ok(!filtered.isError);
    assert.equal((filtered.structuredContent?.query as unknown[]).length,0);
    assert.equal((filtered.structuredContent?.mutation as unknown[]).length,1);
    assert.ok(!('SendInput' in (filtered.structuredContent?.inputObjects as object)));
    const before=calls;rejectOnce=true;const renewed=await call();assert.ok(!renewed.isError);
    assert.equal(calls-before,2);assert.equal(refreshes,1);
    assert.ok((await loadSession(path)).getCookiesSync('https://www.abler.io/graphql').some(c=>c.value==='private-rotated'));
    large=true;const capped=await call({max_chars:1000});assert.ok(!capped.isError);
    assert.equal(capped.structuredContent?.truncated,true);assert.ok(JSON.stringify(capped.structuredContent).length<=1000);
    assert.ok(capped.structuredContent?.note);
    const beforeInvalid=calls;assert.equal((await call({max_chars:1})).isError,true);assert.equal(calls,beforeInvalid);
  }finally{await client.close();await served.close();await rm(directory,{recursive:true,force:true});}
});

test('disabled introspection preserves validation suggestions and never invokes a mutation resolver',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'abler-describe-disabled-'));const path=join(directory,'session.json');
  const cookie={domain:'www.abler.io',path:'/',expires:Date.now()/1000+3600};
  await saveSession(path,await importCookies([{...cookie,name:'refreshToken',value:'private-refresh'},{...cookie,name:'id_token',value:'private-access'}]));
  let writes=0;const operations:string[]=[];
  const served=await serveStdio(path,(url,init)=>{
    assert.ok(url.endsWith('/graphql'));const body=JSON.parse(String(init.body));operations.push(body.operationName);
    if(body.operationName==='DescribeApi')return Response.json({errors:[{message:'Introspection disabled private-access person@example.test 0101991239'}]},{status:400});
    if(body.operationName==='DescribeApiMutationType')return Response.json({data:{__typename:'RootMutation'}});
    assert.match(body.query,/__describe_api_invalid/);assert.match(body.query,/@skip\(if: true\)/);
    if(!body.query.includes('@skip(if: true)'))writes++;
    return Response.json({errors:[{message:'Cannot query field "markConversationRead" on type "RootMutation". Did you mean "markConversationAsRead" or "readConversation"? private-access'}]},{status:400});
  });
  const client=new Client({name:'describe-disabled-test',version:'1.0.0'});await client.connect(served.transport);
  try{
    const response=await client.callTool({name:'describe_api',arguments:{}});assert.ok(!response.isError,JSON.stringify(response));
    assert.equal(response.structuredContent?.introspection,false);assert.equal(response.structuredContent?.mutationType,'RootMutation');
    assert.deepEqual(response.structuredContent?.suggestions,['markConversationAsRead','readConversation']);
    assert.equal(operations.length,5);assert.equal(writes,0);
    assert.ok(!JSON.stringify(response).includes('private-access'));assert.ok(!JSON.stringify(response).includes('person@example.test'));assert.ok(!JSON.stringify(response).includes('0101991239'));
    assert.ok((response.structuredContent?.errors as string[])[0]?.includes('introspection'));
  }finally{await client.close();await served.close();await rm(directory,{recursive:true,force:true});}
});
