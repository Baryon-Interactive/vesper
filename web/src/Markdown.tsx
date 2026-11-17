// Minimal Markdown for store descriptions: headings, paragraphs, lists,
// **bold**, *italic*, `code` and https links. Output is React elements only;
// description text is never inserted as HTML.
import type { ReactNode } from "react";

function inline(text: string, key: string): ReactNode[] {
  const out: ReactNode[] = [];
  const pattern = /(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`|\[[^\]]+\]\(https:\/\/[^)\s]+\))/g;
  let last = 0;
  let n = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index! > last) out.push(text.slice(last, match.index));
    const token = match[0];
    const k = `${key}-${n++}`;
    if (token.startsWith("**")) out.push(<strong key={k}>{token.slice(2, -2)}</strong>);
    else if (token.startsWith("`")) out.push(<code key={k}>{token.slice(1, -1)}</code>);
    else if (token.startsWith("[")) {
      const [, label, href] = /^\[([^\]]+)\]\((.+)\)$/.exec(token)!;
      out.push(
        <a key={k} href={href} target="_blank" rel="noopener noreferrer">
          {label}
        </a>,
      );
    } else out.push(<em key={k}>{token.slice(1, -1)}</em>);
    last = match.index! + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function Markdown({ source, skipTitle }: { source: string; skipTitle?: string }) {
  const blocks: ReactNode[] = [];
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (!line.trim()) {
      i++;
    } else if (heading) {
      const level = Math.min(heading[1].length + 1, 4);
      if (!(level === 2 && skipTitle && heading[2].trim() === skipTitle.trim())) {
        const Tag = `h${level}` as "h2" | "h3" | "h4";
        blocks.push(<Tag key={i}>{inline(heading[2], `h${i}`)}</Tag>);
      }
      i++;
    } else if (/^\s*[-*]\s+/.test(line)) {
      const items: ReactNode[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
        items.push(<li key={i}>{inline(lines[i].replace(/^\s*[-*]\s+/, ""), `l${i}`)}</li>);
        i++;
      }
      blocks.push(<ul key={`ul${i}`}>{items}</ul>);
    } else {
      const para: string[] = [];
      while (i < lines.length && lines[i].trim() && !/^(#{1,4})\s+/.test(lines[i]) && !/^\s*[-*]\s+/.test(lines[i])) {
        para.push(lines[i].trim());
        i++;
      }
      blocks.push(<p key={`p${i}`}>{inline(para.join(" "), `p${i}`)}</p>);
    }
  }
  return <div className="markdown">{blocks}</div>;
}
