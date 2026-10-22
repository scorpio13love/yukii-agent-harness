#!/usr/bin/env node
// 最小 MCP Streamable HTTP server（JSON 响应版）。
// POST /mcp；初始化响应带 Mcp-Session-Id；通知返回 202。
import http from 'node:http';

const tools = [
  {
    name: 'text_stats',
    description: '统计字符数、非空白字符数、单词数和行数。',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string', maxLength: 65536 } },
      required: ['text'],
      additionalProperties: false,
    },
  },
];

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

function handle(message) {
  if (message.method === 'initialize') {
    return {
      protocolVersion: '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'example-http-text-tools', version: '1.0.0' },
    };
  }
  if (message.method === 'tools/list') return { tools };
  if (message.method === 'tools/call') {
    if (message.params?.name !== 'text_stats') throw new Error('unknown tool');
    return {
      isError: false,
      content: [{ type: 'text', text: JSON.stringify(textStats(message.params?.arguments ?? {})) }],
    };
  }
  throw new Error('method not found');
}

const server = http.createServer((request, response) => {
  const isSse = request.url === '/mcp-sse';
  if (request.method !== 'POST' || (!isSse && request.url !== '/mcp')) {
    response.writeHead(405).end();
    return;
  }
  let body = '';
  request.on('data', (chunk) => {
    body += chunk;
    if (body.length > 2 * 1024 * 1024) request.destroy();
  });
  request.on('end', () => {
    let message;
    try {
      message = JSON.parse(body);
    } catch {
      response.writeHead(400, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify({ error: 'invalid json' }));
      return;
    }
    if (message.id === undefined) {
      response.writeHead(202).end();
      return;
    }
    response.setHeader('Mcp-Session-Id', 'demo-session');
    let payload;
    try {
      const result = handle(message);
      payload = { jsonrpc: '2.0', id: message.id, result };
    } catch (error) {
      payload = {
        jsonrpc: '2.0',
        id: message.id,
        error: { code: -32601, message: String(error.message ?? error) },
      };
    }
    if (isSse) {
      response.setHeader('Content-Type', 'text/event-stream');
      response.setHeader('Cache-Control', 'no-cache');
      response.end(`event: message\ndata: ${JSON.stringify(payload)}\n\n`);
    } else {
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify(payload));
    }
  });
});

server.listen(3333, '127.0.0.1', () => {
  process.stderr.write('MCP HTTP server listening on http://127.0.0.1:3333/mcp\n');
});
