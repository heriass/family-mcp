import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'bun:test';
import { Client } from '@modelcontextprotocol/client';
import { CookieJar } from 'tough-cookie';
import { captureSession, PARENT_URL, writeSession } from '../../../../packages/infomentor-mcp/src/session.js';
import { useScratchStore } from '../../../../packages/infomentor-mcp/test/scratch.js';
import { serveStdio } from './rust-infomentor.js';

useScratchStore();

test('full school readers discover only read actions and return complete text, links, dates and metadata', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'school-readers-'));
  const file = join(directory, 'session.json');
  const jar = new CookieJar();
  await jar.setCookie('IMHome=synthetic; Secure; HttpOnly; Path=/', PARENT_URL);
  await writeSession(await captureSession(jar), file);
  let selected = 'first';
  let mode = 'normal';
  const seen: string[] = [];
  const appNames = ['calendarv2','calendar','homework','task','communication','grouptimeline','message','assessmentv2'];
  const keyNames: Record<string,string[]> = {
    calendarv2:['getEntries','getEntry','getAttachments'], calendar:['getEntries','getEntry','getAttachments'],
    homework:['getMessages','getMessage','getMoreItems'], task:['getTasks','getTask','getUol'],
    communication:['getNewsList','getDocumentsList','getLinksList','itemUrl'], grouptimeline:['getNewsList','itemUrl'],
    message:['getMessages','getMessage'], assessmentv2:['getNavigationData','getGridItems','getTaskItems','getUolItems','getLearningStepItems','getSelfAssessmentItems'],
  };
  const summary = {id:41,messageContextType:'General',sentUser:{id:12,displayName:'Teacher'},isNew:true,messageSubject:'Read this',timeSent:'12.10.2026 09:00'};
  const detail = {title:'Full title',description:'<p>Full first paragraph.</p><ul><li>Second point</li></ul><a href="/docs/1?token=private">Read notes</a>',location:'Room 2',start:'2026-10-12T09:00:00',dueDate:'13.10.2026 10:30',status:'Assigned',teacherComment:'<p>Complete comment</p>',grade:'A',criteria:[{title:'Reading',achieved:true}],kennitala:'0101991239',oauth_token:'private'};
  const served = await serveStdio({sessionFile:file, fetch:(url,init)=> {
    const path = url.pathname.toLowerCase();
    seen.push(`${init.method} ${path}`);
    if(path === '/') return new Response(`<script>IMHome.home.homeData = ${JSON.stringify({account:{currentUser:{id:'parent'},pupils:['first','second'].map((id,index)=>({id,name:id,selected:id===selected,switchPupilUrl:`/Account/PupilSwitcher/SwitchPupil/${101+index}`}))},apps:(mode === 'disabled' ? [] : appNames).map(codeName=>({codeName}))})}; IMHome.home.init(1);</script>`);
    if(path === '/account/pupilswitcher/switchpupil/102') { selected='second'; return new Response(null,{status:302,headers:{Location:PARENT_URL}}); }
    assert.equal(init.method,'POST');
    assert.match(new Headers(init.headers).get('cookie') ?? '', /IMHome=synthetic/);
    if(path === '/home/home/appdata') return Response.json({homeData:{urls:mode==='homeUnavailable'?{}:{notificationAppData:'/NotificationApp/NotificationApp/appData'}}});
    if(path === '/notificationapp/notificationapp/appdata') return Response.json({notifications:[{id:1,title:'Notice',subTitle:'Legacy notice body',subjectsCourses:'',dateSent:'12.10.2026',appType:'Message',state:'New',type:'MessageCreated',url:'/#/message/show/41',pupilIM2Id:1,pupilSourceId:'synthetic-child',currentlySelectedPupil:true,body:'<p>Full announcement</p>'}]});
    if(path === '/message/message/getmessages') return Response.json({items:[summary],more:false});
    if(path === '/message/message/getmessage') return Response.json({...summary,messageBody:'<p>Full message including all paragraphs.</p>',messageBodyPlainText:'Full message including all paragraphs.',toUsers:[{id:13,displayName:'Parent'}],messageFolder:'Inbox',attachments:[{name:'Sheet.pdf',type:'application/pdf',size:45,downloadUrl:'/docs/1?token=private'}]});
    const [,app,controller,action,id] = path.split('/');
    const communicationList = app === 'communication' && ['getnewslist','getdocumentslist','getlinkslist'].includes(action);
    assert.ok(app === controller || communicationList);
    if(action === 'appdata') {
      if(mode==='write302') return new Response(null,{status:302,headers:{Location:`/${app}/${app}/Update`}});
      const keys = (mode === 'fallback' && app === 'calendarv2') ? [] : keyNames[app!] ?? [];
      const urls = Object.fromEntries(keys.map(key=>[key,app === 'communication' && key !== 'itemUrl' ? `/Communication/${key === 'getNewsList' ? 'News' : key === 'getDocumentsList' ? 'Documents' : 'Links'}/${key}` : `/${app}/${app}/${key === 'itemUrl' ? 'Item' : key}`]));
      urls.sendMessage=`/${app}/${app}/SendMessage`;
      urls.externalUpdateUrl=`/${app}/${app}/Update`;
      return Response.json({urls,gridItemsList:[{id:41,isNew:true}],uolItemsList:[],selfAssessmentItemsList:[],items:[{id:41}]});
    }
    assert.equal(new Headers(init.headers).get('content-type'),'application/json');
    if(mode === 'loginRedirect') return new Response(null,{status:302,headers:{Location:'/Authentication/Authentication/Login'}});
    if(id) {
      assert.equal(init.body ?? '','');
      assert.equal(new Headers(init.headers).get('content-length'),'0');
      if(action === 'getattachments') return Response.json({attachments:[{name:'Notes.pdf',type:'application/pdf',size:99,downloadUrl:'/docs/42?token=private'}]});
      if(id === 'objective-1') return Response.json({id,description:'Full learning objective'});
      if(id === '42') return Response.json({id:42,description:'Linked assessment full text',linkedGrids:[{id:43}]});
      return Response.json({id:41,...detail,description:mode==='large'?detail.description+'x'.repeat(3000):detail.description,linkedUols:[{id:'objective-1'}],linkedTasks:app==='assessmentv2'?[{id:42}]:[]});
    }
    const filters = JSON.parse(String(init.body ?? '{}'));
    if(path === '/task/task/gettasks') assert.deepEqual(filters,{});
    if(app === 'calendarv2' || app === 'calendar') {
      assert.deepEqual(filters,{startDate:'2026-10-01',endDate:'2026-10-31'});
      return Response.json({entries:[{id:41,title:'Entry',start:'2026-10-12T09:00:00'}],more:false});
    }
    return Response.json({items:[{id:41}],page:filters.page,more:false});
  }});
  const client = new Client({name:'school-tools-test',version:'1.0.0'});
  await client.connect(served.transport);
  const call = (name:string,args:Record<string,unknown>={})=>client.callTool({name,arguments:args});
  try {
    assert.equal((await client.listTools()).tools.length,18);
    const discovery=await call('infomentor_app_data',{app:'task'});
    assert.ok(!discovery.isError);
    assert.equal(Object.keys(discovery.structuredContent ?? {})[0],'urls');
    assert.equal((discovery.structuredContent?.urls as Record<string,{readable:boolean}>).sendMessage?.readable,false);
    const before=seen.length;
    assert.equal((await call('infomentor_app_get',{app:'task',url_key:'sendMessage',body:{}})).isError,true);
    assert.equal((await call('infomentor_app_get',{app:'task',url_key:'getTask',id:41,body:{markRead:true}})).isError,true);
    assert.equal(seen.length,before);
    mode='write302';
    assert.equal((await call('infomentor_app_data',{app:'task'})).isError,true);
    mode='normal';
    const direct=await call('infomentor_app_get',{app:'task',url_key:'getTask',id:41,childId:'second'});
    assert.ok(!direct.isError);assert.equal(direct.structuredContent?.childId,'second');
    for(const name of ['infomentor_get_calendar','infomentor_get_homework','infomentor_get_assignments','infomentor_get_news']) {
      const response=await call(name,name==='infomentor_get_calendar'?{from_date:'2026-10-01',to_date:'2026-10-31'}:{page:1});
      assert.ok(!response.isError,JSON.stringify(response));
      assert.match(JSON.stringify(response),/Full first paragraph/);
      assert.match(JSON.stringify(response),/Second point/);
      if(name==='infomentor_get_assignments') assert.match(JSON.stringify(response),/Full learning objective/);
      assert.ok(!JSON.stringify(response).includes('private'));
      assert.ok(!JSON.stringify(response).includes('0101991239'));
    }
    assert.ok(seen.includes('POST /calendarv2/calendarv2/getentries'));
    const assignments=await call('infomentor_get_assignments');
    assert.ok(!assignments.isError);assert.ok(seen.includes('POST /task/task/gettasks'));
    mode='fallback';
    const unavailableCalendar=await call('infomentor_get_calendar',{from_date:'2026-10-01',to_date:'2026-10-31'});
    assert.ok(!unavailableCalendar.isError);assert.equal(unavailableCalendar.structuredContent?.available,false);mode='normal';
    for(const name of ['infomentor_get_calendar_entry','infomentor_get_homework_item','infomentor_get_assignment']) {
      const response=await call(name,{id:41});assert.ok(!response.isError,JSON.stringify(response));
      assert.match(JSON.stringify(response),/2026-10-12T09:00:00.000Z/);
      assert.match(JSON.stringify(response),/2026-10-13T10:30:00.000Z/);
      if(name==='infomentor_get_calendar_entry') assert.match(JSON.stringify(response),/Notes.pdf/);
    }
    assert.ok(seen.includes('POST /task/task/gettask/41'));
    const assessment=await call('infomentor_get_assessment_item',{type:'grid',id:41});
    assert.ok(!assessment.isError);assert.match(JSON.stringify(assessment),/Linked assessment full text/);
    assert.ok(!seen.some(path=>path.endsWith('/43')));
    const messages=await call('infomentor_get_messages');
    assert.ok(!messages.isError);assert.match(JSON.stringify(messages.structuredContent?.fullContent),/Full message including all paragraphs/);
    const message=await call('infomentor_get_message',{id:41});
    assert.ok(!message.isError);assert.match(JSON.stringify(message),/Sheet.pdf/);
    mode='large';
    const capped=await call('infomentor_get_calendar',{from_date:'2026-10-01',to_date:'2026-10-31',max_chars:1000});
    assert.ok(!capped.isError);assert.equal(capped.structuredContent?.truncated,true);
    assert.ok((capped.structuredContent?.json as string).length<=1000);assert.ok(capped.structuredContent?.next);
    mode='normal';
    const notifications=await call('infomentor_get_notifications');
    assert.ok(!notifications.isError);assert.match(JSON.stringify(notifications.structuredContent?.fullContent),/Full announcement/);
    mode='homeUnavailable';
    const legacyNotifications=await call('infomentor_get_notifications');
    assert.ok(!legacyNotifications.isError);assert.match(JSON.stringify(legacyNotifications.structuredContent),/Notice/);
    mode='normal';
    const news=await call('infomentor_get_news');
    assert.ok(!news.isError);
    for(const key of ['getNewsList','getDocumentsList','getLinksList']) {
      const response=await call('infomentor_app_get',{app:'communication',url_key:key,body:{}});
      assert.ok(!response.isError,JSON.stringify(response));
    }
    mode='loginRedirect';
    const expiredRead=await call('infomentor_get_calendar',{from_date:'2026-10-01',to_date:'2026-10-31'});
    assert.ok(expiredRead.isError);assert.match(JSON.stringify(expiredRead),/session expired/i);
    mode='normal';
    const truncated=await call('infomentor_app_data',{app:'task',max_chars:1000});
    assert.ok(!truncated.isError);
    mode='disabled';
    const disabled=await call('infomentor_get_homework');assert.ok(!disabled.isError);assert.equal(disabled.structuredContent?.available,false);
    assert.ok(!seen.some(path=>/\/send|\/save|\/update|\/delete|\/mark|\/set|\/post|\/upload|\/reply|\/create|\/remove/.test(path)));
  } finally {await client.close();await served.close();await rm(directory,{recursive:true,force:true});}
});

test('expired school reads attempt at most one renewal per call and never leak credentials',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'school-expired-'));
  const file=join(directory,'session.json');const privateFile=join(directory,'credentials.json');
  const jar=new CookieJar();await jar.setCookie('IMHome=expired; Secure; HttpOnly; Path=/',PARENT_URL);
  await writeSession({...await captureSession(jar),accountId:'parent',selectedChildId:'first'},file);
  await writeFile(privateFile,JSON.stringify({username:'synthetic-user',password:'synthetic-secret'}),{mode:0o600});
  const seen:string[]=[];
  const served=await serveStdio({sessionFile:file,credentialsFile:privateFile,fetch:(url,init)=>{
    seen.push(`${init.method} ${url.pathname}`);
    if(url.pathname==='/')return new Response(null,{status:302,headers:{Location:'/Authentication/Authentication/Login'}});
    return new Response('<form><input type=password></form>');
  }});
  const client=new Client({name:'expired-school-test',version:'1.0.0'});await client.connect(served.transport);
  try {
    for(const name of ['infomentor_app_data','infomentor_get_homework','infomentor_get_messages','infomentor_get_notifications']) {
      const response=await client.callTool({name,arguments:name==='infomentor_app_data'?{app:'task'}:{}});
      assert.equal(response.isError,true);assert.ok(!JSON.stringify(response).includes('synthetic-secret'));
    }
    // fix7: school reads renew like get_overview: at most one sign-in submission per call, never a loop.
    assert.ok(seen.filter(path=>path.startsWith('POST')).length<=4,JSON.stringify(seen));
    assert.ok(seen.filter(path=>path==='GET /').length>=4,JSON.stringify(seen));
  }finally{await client.close();await served.close();await rm(directory,{recursive:true,force:true});}
});
