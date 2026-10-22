// Yukii 容器工具协议：stdin 接收一个 JSON 参数对象，stdout 只输出一个 JSON 结果。
// 这个文件不需要 Rust；工具运行时只依赖 Node。
type TextStatsInput = {
  text: string;
  mode?: 'all' | 'summary';
};

type TextStatsOutput = {
  characters: number;
  characters_without_spaces: number;
  words: number;
  lines: number;
  mode: 'all' | 'summary';
};

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString('utf8');
}

function fail(message: string): never {
  process.stderr.write(`${message}\n`);
  process.exit(2);
}

async function main(): Promise<void> {
  let input: unknown;
  try {
    input = JSON.parse(await readStdin());
  } catch {
    fail('input must be a JSON object');
  }

  if (typeof input !== 'object' || input === null || Array.isArray(input)) {
    fail('input must be a JSON object');
  }

  const value = input as Partial<TextStatsInput>;
  if (typeof value.text !== 'string') {
    fail('text must be a string');
  }
  if (value.mode !== undefined && value.mode !== 'all' && value.mode !== 'summary') {
    fail('mode must be all or summary');
  }
  if (value.text.length > 64 * 1024) {
    fail('text is too large');
  }

  const text = value.text;
  const output: TextStatsOutput = {
    characters: [...text].length,
    characters_without_spaces: [...text.replace(/\s/g, '')].length,
    words: text.trim() ? text.trim().split(/\s+/).length : 0,
    lines: text.length ? text.split(/\r?\n/).length : 0,
    mode: value.mode ?? 'all',
  };

  process.stdout.write(`${JSON.stringify(output)}\n`);
}

void main();
