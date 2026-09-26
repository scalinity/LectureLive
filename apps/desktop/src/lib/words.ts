// Word IDs for the open utterance (spec §9.3): an unchanged prefix keeps its IDs, so a word turning
// from tentative to stable is the same element; a replaced word gets a new ID and fades in.

export type Word = { id: number; text: string; stable: boolean };

const split = (s: string) => s.split(/\s+/).filter(Boolean);

export function nextWords(prev: Word[], stable: string, tentative: string, fresh: () => number): Word[] {
  const next = [...split(stable).map((text) => ({ text, stable: true })), ...split(tentative).map((text) => ({ text, stable: false }))];
  let same = true;
  return next.map((w, i) => {
    same = same && prev[i]?.text === w.text;
    return { id: same ? prev[i].id : fresh(), ...w };
  });
}
