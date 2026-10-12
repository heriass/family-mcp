import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'bun:test';
import { Client } from '@modelcontextprotocol/client';
import { importCookies, saveSession } from '../../../../packages/abler-mcp/src/auth.js';
import { useScratchStore } from '../../../../packages/abler-mcp/test/scratch.js';
import { serveStdio } from './rust-abler.js';

useScratchStore();

test('guarded discovery supports both root defaults and explicit fields without resolver execution',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'abler-field-discovery-'));
  const path=join(directory,'session.json');
  const cookie={domain:'www.abler.io',path:'/',expires:Date.now()/1000+3600};
  await saveSession(path,await importCookies([{...cookie,name:'refreshToken',value:'private-refresh'},{...cookie,name:'id_token',value:'private-access'}]));
  const documents:string[]=[];let root='Mutation';let requests=0;
  const served=await serveStdio(path,(url,init)=>{
    assert.ok(url.endsWith('/graphql'));const body=JSON.parse(String(init.body));requests++;
    if(body.operationName==='DescribeApi')return Response.json({errors:[{message:'Introspection disabled private-access person@example.test 0101991239'}]},{status:400});
    documents.push(body.query);assert.match(body.query,/__describe_api_invalid/);assert.match(body.query,/@skip\(if: true\)/);
    assert.ok(body.query.startsWith(root==='Query'?'query ':'mutation '));
    const field=/__describe_api_invalid\s+(\w+)/.exec(body.query)?.[1];assert.ok(field);
    const errors=[{message:`Cannot query field "__describe_api_invalid" on type "Root${root}".`}];
    if(field==='MessageReply') {
      errors.push({message:'Field "MessageReply" must have a selection of subfields. Did you mean "MessageReply { ... }"?'});
      errors.push({message:body.query.includes('(input: {})')?'Field "text" of required type "String!" was not provided.':'Field "MessageReply" argument "input" of type "ReplyInput!" is required, but it was not provided.'});
    } else if(field==='MarkAsRead')errors.push({message:'Field "MarkAsRead" argument "conversationId" of type "ID!" is required, but it was not provided.'});
    else if(field!=='__typename') errors.push({message:`Cannot query field "${field}" on type "Root${root}". Did you mean "${root==='Query'?'feedItems':'MarkAsRead'}"? private-access`});
    return Response.json({errors},{status:400});
  });
  const client=new Client({name:'field-discovery-test',version:'1.0.0'});await client.connect(served.transport);
  const call=(args:Record<string,unknown>={})=>client.callTool({name:'describe_api',arguments:args});
  try {
    let before=requests;const mutation=await call();assert.ok(!mutation.isError,JSON.stringify(mutation));
    assert.equal(requests-before,30);assert.equal(mutation.structuredContent?.root,'Mutation');
    assert.equal((mutation.structuredContent?.probes as object[]).length,29);
    assert.ok(documents.some(query=>query.includes('SetAttendance @skip')));assert.ok(documents.some(query=>query.includes('UpdateNotificationSettings @skip')));
    root='Query';before=requests;const query=await call({root:'Query'});assert.ok(!query.isError,JSON.stringify(query));
    assert.equal(requests-before,19);assert.equal(query.structuredContent?.queryType,'RootQuery');assert.equal(query.structuredContent?.mutationType,null);
    assert.deepEqual(query.structuredContent?.suggestions,['feedItems']);
    root='Mutation';before=requests;const fields=await call({probes:[],describe_fields:['MarkAsRead','MessageReply']});assert.ok(!fields.isError,JSON.stringify(fields));
    assert.equal(requests-before,5);
    const described=fields.structuredContent?.describedFields as {field:string,requiredArguments:{name:string,type:string}[],requiredInputFields:{name:string,type:string}[],selectionSetRequired:boolean|null,stages:object[]}[];
    assert.equal(described[0]?.requiredArguments[0]?.name,'conversationId');assert.equal(described[0]?.requiredArguments[0]?.type,'ID!');
    assert.equal(described[1]?.requiredArguments[0]?.type,'ReplyInput!');assert.equal(described[1]?.requiredInputFields[0]?.name,'text');
    assert.equal(described[1]?.selectionSetRequired,true);assert.equal(described[1]?.stages.length,2);
    assert.ok(!JSON.stringify(fields).includes('private-access'));assert.ok(!JSON.stringify(fields).includes('person@example.test'));assert.ok(!JSON.stringify(fields).includes('0101991239'));
    before=requests;
    for(const args of [{probes:['x } mutation { write']},{probes:Array(61).fill('field')},{describe_fields:Array(21).fill('field')},{describe_fields:['x'.repeat(65)]},{root:'Other'}]) assert.equal((await call(args)).isError,true);
    assert.equal(requests,before);
    const capped=await call({max_chars:1000});assert.ok(!capped.isError);assert.equal(capped.structuredContent?.truncated,true);assert.ok(JSON.stringify(capped.structuredContent).length<=1000);
    assert.ok(documents.every(query=>query.includes('__describe_api_invalid')&&query.includes('@skip(if: true)')));
  }finally{await client.close();await served.close();await rm(directory,{recursive:true,force:true});}
});
