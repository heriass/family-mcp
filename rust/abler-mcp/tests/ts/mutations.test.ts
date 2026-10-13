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

test('fixed Abler mutations require previews, linked child IDs and discovered attendance enum values', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-mutations-'));
  const path = join(directory, 'session.json');
  const cookie = { domain: 'www.abler.io', path: '/', expires: Date.now() / 1000 + 3600 };
  await saveSession(path, await importCookies([
    { ...cookie, name: 'refreshToken', value: 'private-refresh' },
    { ...cookie, name: 'id_token', value: 'private-access' },
  ]));
  const writes: { operation: string; query: string; variables: Record<string, unknown> }[] = [];
  const enumProbes: string[] = [];
  const served = await serveStdio(path, (url, init) => {
    assert.ok(url.endsWith('/graphql'));
    const body = JSON.parse(String(init.body)) as { operationName: string; query: string; variables: Record<string, unknown> };
    if (body.operationName === 'Profile') return Response.json({ data: { me: { id: 'user-7', displayName: 'Synthetic User', children: [{ id: 'child-1', displayName: 'Synthetic Child' }] } } });
    if (body.operationName === 'ValidateAttendanceStatus' || body.operationName === 'DescribeAttendanceEnum') {
      enumProbes.push(body.query);
      assert.match(body.query, /__describe_api_invalid/);
      assert.match(body.query, /setPlayerAttendanceV4\(eventId: "0", playerId: "0", status: __INVALID_ENUM__\) @skip\(if: true\)/);
      return Response.json({ errors: [
        { message: 'Cannot query field "__describe_api_invalid" on type "Mutation".' },
        { message: 'Value "__INVALID_ENUM__" does not exist in "PLAYER_ATTENDANCE_STATUS" enum. Did you mean "PRESENT", "ABSENT", or "LATE"?' },
      ] }, { status: 400 });
    }
    writes.push(body);
    const result: Record<string, unknown> = body.operationName === 'MarkAllAsRead'
      ? { MarkAllAsRead: true }
      : { [body.operationName]: { __typename: 'ActionResult' } };
    return Response.json({ data: result });
  });
  const client = new Client({ name: 'mutation-policy-test', version: '1.0.0' });
  await client.connect(served.transport);
  const call = (name: string, args: Record<string, unknown> = {}) => client.callTool({ name, arguments: args });
  try {
    const tools = (await client.listTools()).tools;
    const actionNames = ['mark_conversation_read', 'mark_all_read', 'reply_to_conversation', 'set_child_attendance', 'join_event', 'leave_event'];
    assert.deepEqual(tools.filter((tool) => actionNames.includes(tool.name)).map((tool) => tool.name).toSorted(), actionNames.toSorted());
    for (const tool of tools.filter((item) => actionNames.includes(item.name))) {
      assert.equal(tool.annotations?.readOnlyHint, false);
      assert.equal(tool.annotations?.destructiveHint, false);
    }

    const reply = await call('reply_to_conversation', { conversationId: 'conversation-1', messageText: 'Hello' });
    assert.equal(reply.structuredContent?.preview, true);
    assert.equal(reply.structuredContent?.operationName, 'MessageReply');
    assert.equal((reply.structuredContent?.variables as Record<string, unknown>).messageText, 'Hello');
    assert.match(String(reply.structuredContent?.message), /ask the user to approve/);
    assert.equal(writes.length, 0);
    assert.equal((await call('reply_to_conversation', { conversationId: 'conversation-1', messageText: 'Hello', confirm: false })).structuredContent?.preview, true);
    assert.equal(writes.length, 0);

    const attendance = await call('set_child_attendance', { eventId: 'event-1', childId: 'child-1', status: 'PRESENT' });
    assert.equal(attendance.structuredContent?.preview, true);
    assert.equal(writes.length, 0);
    assert.equal((await call('set_child_attendance', { eventId: 'event-1', childId: 'unknown-child', status: 'PRESENT', confirm: true })).isError, true);
    assert.equal(writes.length, 0);
    assert.equal((await call('set_child_attendance', { eventId: 'event-1', childId: 'child-1', status: 'UNLISTED', confirm: true })).isError, true);
    assert.equal(writes.length, 0);

    for (const name of ['join_event', 'leave_event']) {
      const args = { childId: 'child-1', eventId: 'event-1', ageGroupId: 'group-1' };
      const preview = await call(name, args);
      assert.equal(preview.structuredContent?.preview, true);
      assert.equal(writes.length, 0);
      const denied = await call(name, { ...args, childId: 'unknown-child', confirm: true });
      assert.equal(denied.isError, true);
      assert.equal(writes.length, 0);
    }

    const replySent = await call('reply_to_conversation', { conversationId: 'conversation-1', messageText: 'Hello', confirm: true });
    assert.equal(replySent.structuredContent?.success, true);
    const attendanceSent = await call('set_child_attendance', { eventId: 'event-1', childId: 'child-1', status: 'PRESENT', confirm: true });
    assert.equal(attendanceSent.structuredContent?.success, true);
    for (const name of ['join_event', 'leave_event']) {
      const result = await call(name, { childId: 'child-1', eventId: 'event-1', ageGroupId: 'group-1', confirm: true });
      assert.equal(result.structuredContent?.success, true);
    }
    assert.equal((await call('mark_conversation_read', { conversationId: 'conversation-1' })).structuredContent?.success, true);
    assert.equal((await call('mark_all_read')).structuredContent?.success, true);

    const allowed = ['MarkAsRead', 'MarkAllAsRead', 'MessageReply', 'setPlayerAttendanceV4', 'UserJoinEvent', 'UserLeaveEvent'];
    assert.ok(writes.length > 0);
    assert.ok(writes.every((write) => allowed.includes(write.operation)));
    assert.deepEqual(writes.map((write) => write.operation), ['MessageReply', 'setPlayerAttendanceV4', 'UserJoinEvent', 'UserLeaveEvent', 'MarkAsRead', 'MarkAllAsRead']);
    assert.equal(writes.find((write) => write.operation === 'MarkAllAsRead')?.variables.userId, 'user-7');
    assert.equal(writes.find((write) => write.operation === 'setPlayerAttendanceV4')?.variables.playerId, 'child-1');
    assert.equal(writes.find((write) => write.operation === 'UserJoinEvent')?.variables.toEventId, 'event-1');
    assert.ok(writes.every((write) => write.query.includes('{ __typename }') || write.operation === 'MarkAllAsRead'));
    assert.equal(enumProbes.length, 2);
  } finally {
    await client.close();
    await served.close();
    await rm(directory, { recursive: true, force: true });
  }
});
