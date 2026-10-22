#!/usr/bin/env node
// 最小 MCP stdio server：initialize / tools/list / tools/call。
// stdout 只允许 JSON-RPC；日志写到 stderr。
import readline from 'node:readline';

const tools = [
  {
    name: 'text_stats',
    description: '统计字符数、非空白字符数、单词数和行数。',
    inputSchema: {
      type: 'object',
      properties: {
        text: { type: 'string', maxLength: 65536 },
      },
      required: ['text'],
      additionalProperties: false,
    },
  },
];

function reply(id, result) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, result })}\n`);
}

function fail(id, code, message) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } })}\n`);
}

function textStats(input) {
  if (typeof input?.text !== 'string') throw new Error('text must be a string');
  if (input.text.length > 65536) throw new Error('text is too large');
  const text = input.text;
  return {
    characters: [...text].length,
    characters_without_spaces: [...text.replace(/\s/g, '')].length,
    words: text.trim() ? text.trim().split(/\s+/).length : 0,
    lines: text.length ? text.split(/\r?\n/).length : 0,
  };
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on('line', (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    process.stderr.write('invalid json\n');
    return;
  }
  if (message.method === 'notifications/initialized') return;
  if (message.method === 'initialize') {
    reply(message.id, {
      protocolVersion: '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'example-text-tools', version: '1.0.0' },
    });
    return;
  }
  if (message.method === 'tools/list') {
    reply(message.id, { tools });
    return;
  }
  if (message.method === 'tools/call') {
    try {
      if (message.params?.name !== 'text_stats') throw new Error('unknown tool');
      const result = textStats(message.params?.arguments ?? {});
      reply(message.id, {
        isError: false,
        content: [{ type: 'text', text: JSON.stringify(result) }],
      });
    } catch (error) {
      reply(message.id, {
        isError: true,
        content: [{ type: 'text', text: String(error.message ?? error) }],
      });
    }
    return;
  }
  if (message.id !== undefined) fail(message.id, -32601, 'method not found');
});
