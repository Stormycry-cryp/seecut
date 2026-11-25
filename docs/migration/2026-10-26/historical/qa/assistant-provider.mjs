// SPDX-License-Identifier: AGPL-3.0-or-later
// Loopback-only synthetic provider for manual testing through the normal App.
// Uses no external services. Logs event counts, never prompts or credentials.
import { createServer } from 'node:http';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const output = resolve(process.argv[2] ?? 'assistant-provider-evidence');
mkdirSync(output, { recursive: true });
const sockets = new Set();
const timers = new Set();
let requests = 0;
let host;
const log = (event, fields = {}) => appendFileSync(`${output}/provider-events.jsonl`,
  JSON.stringify({ at: new Date().toISOString(), event, ...fields }) + '\n');
const server = createServer({ maxHeaderSize: 8192 }, async (req, res) => {
  const id = ++requests;
  const reject = (status, code) => {
    log('rejected', { id, status, code });
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: { message: code, type: 'synthetic_fixture' } }));
  };
  if (id > 30 || req.socket.remoteAddress !== '127.0.0.1' || req.headers.host !== host)
    return reject(403, 'fixture_limit_or_host');
  if (req.method !== 'POST' || req.url !== '/v1/chat/completions')
    return reject(404, 'fixture_route');
  if (req.headers.authorization !== 'Bearer local-qa-synthetic')
    return reject(401, 'synthetic_key_required');
  let body;
  try {
    const chunks = [];
    let size = 0;
    for await (const chunk of req) {
      size += chunk.length;
      if (size > 1048576) { req.destroy(); return; }
      chunks.push(chunk);
    }
    body = JSON.parse(Buffer.concat(chunks));
  } catch { return reject(400, 'fixture_json'); }
  if (body.model !== 'local-qa' || body.stream !== true || !Array.isArray(body.messages))
    return reject(422, 'fixture_protocol');
  const lastUser = body.messages.findLastIndex(message => message.role === 'user');
  const content = body.messages[lastUser]?.content;
  const prompt = typeof content === 'string' ? content
    : Array.isArray(content) && content.every(part => part?.type === 'text' && typeof part.text === 'string')
      ? content.map(part => part.text).join('\n') : null;
  if (typeof prompt !== 'string') return reject(422, 'text_input_only');
  if (prompt.trim() === 'fail') return reject(503, 'intentional_local_failure');
  const hasToolResult = body.messages.slice(lastUser + 1).some(message => message.role === 'tool');
  const tools = prompt.trim() === 'tools';
  const streaming = prompt.trim() === 'stream';
  if (tools && !hasToolResult && !body.tools?.some(tool => tool.function?.name === 'capabilities'))
    return reject(422, 'capabilities_unavailable');
  const envelope = (delta, finish_reason = null) => ({ id: `local-qa-${id}`,
    object: 'chat.completion.chunk', created: 1, model: 'local-qa',
    choices: [{ index: 0, delta, finish_reason }] });
  const emit = (delta, finish) => res.write(`data: ${JSON.stringify(envelope(delta, finish))}\n\n`);
  const finish = reason => { emit({}, reason); res.end('data: [DONE]\n\n'); };
  res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
  log('accepted', { id, scenario: tools ? 'tools' : streaming ? 'stream' : 'quick', hasToolResult });
  if (tools && !hasToolResult) {
    emit({ role: 'assistant', tool_calls: [{ index: 0, id: `qa-capabilities-${id}`,
      type: 'function', function: { name: 'capabilities', arguments: '{}' } }] });
    finish('tool_calls');
    log('tool_requested', { id, name: 'capabilities' });
    return;
  }
  emit({ role: 'assistant', content: tools
    ? '已检查本地编辑器支持的操作。下面是给你的回复正文。\n'
    : '这是本地测试服务生成的回复，用于检查阅读和输入操作。\n' });
  let ticks = 0;
  const timer = setInterval(() => {
    ticks++;
    emit({ content: `第 ${ticks} 段回复：检查流式追加时，上方阅读位置是否保留。\n` });
    if (ticks === (streaming ? 80 : 4)) {
      clearInterval(timer); timers.delete(timer);
      finish('stop'); log('completed', { id, ticks });
    }
  }, streaming ? 250 : 150);
  timers.add(timer);
  res.once('close', () => {
    clearInterval(timer); timers.delete(timer);
    if (ticks < (streaming ? 80 : 4)) log('stream_closed', { id, ticks });
  });
});
server.requestTimeout = 10000;
server.on('connection', socket => {
  sockets.add(socket);
  socket.once('close', () => sockets.delete(socket));
});
function close() {
  for (const timer of timers) clearInterval(timer);
  for (const socket of sockets) socket.destroy();
  server.close();
  log('closed', { requests });
}
process.once('SIGINT', close);
process.once('SIGTERM', close);
const deadline = setTimeout(close, 15 * 60 * 1000);
server.once('close', () => clearTimeout(deadline));
const requestedPort = Number(process.argv[3] ?? 0);
if (!Number.isInteger(requestedPort) || requestedPort < 0 || requestedPort > 65535)
  throw new Error('invalid_loopback_port');
server.listen(requestedPort, '127.0.0.1', () => {
  host = `127.0.0.1:${server.address().port}`;
  const connection = { baseUrl: `http://${host}/v1`, model: 'local-qa',
    syntheticKey: 'local-qa-synthetic', prompts: ['stream', 'tools', 'fail'],
    deadlineMinutes: 15, limits: 'synthetic isolated App only; capabilities is read-only' };
  writeFileSync(`${output}/provider-connection.json`, JSON.stringify(connection, null, 2) + '\n');
  log('listening', { baseUrl: connection.baseUrl });
  console.log(JSON.stringify(connection));
});
