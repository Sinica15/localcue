export interface TextMapper {
  length(): number;
  posToY(pos: number): number;
  yToPos(y: number): number;
  lineStart(i: number): number;
}
export function createTextMapper(
  el: HTMLElement,
  lineHeightFn: () => number,
  toStage?: (r: DOMRect) => { top: number; height: number },
): TextMapper;
export const READING_LINE: number;
