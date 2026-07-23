import { useEffect, useRef } from "react";
import type { Buffer } from "../state/store";

const MIRC_COLORS = [
  "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000",
  "#9c009c", "#fc7f00", "#ffff00", "#00fc00", "#009393", "#00ffff",
  "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
];

function color(value: string): string {
  const numeric = Number(value);
  if (Number.isInteger(numeric) && numeric >= 0 && numeric < MIRC_COLORS.length) {
    return MIRC_COLORS[numeric];
  }
  if (Number.isInteger(numeric) && numeric >= 0) {
    const red = numeric & 0xff;
    const green = (numeric >> 8) & 0xff;
    const blue = (numeric >> 16) & 0xff;
    return `rgb(${red},${green},${blue})`;
  }
  return value || "#ffffff";
}

export function PictureWindow({ buffer }: { buffer: Buffer }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const drawing = buffer.windowDrawing ?? [];

  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;
    const size = [...drawing].reverse().find((item) => item.op === "drawsize");
    const width = Math.min(8192, Math.max(1, Number(size?.args[1]) || canvas.clientWidth || 640));
    const height = Math.min(8192, Math.max(1, Number(size?.args[2]) || canvas.clientHeight || 400));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    context.clearRect(0, 0, width, height);

    for (const draw of drawing) {
      const [switches, colour, stroke, ...values] = draw.args;
      const numbers = values.map(Number);
      context.strokeStyle = color(colour);
      context.fillStyle = color(colour);
      context.lineWidth = Math.max(1, Number(stroke) || 1);
      if (draw.op === "drawdot") {
        for (let index = 0; index + 1 < numbers.length; index += 2) {
          context.beginPath();
          context.arc(numbers[index], numbers[index + 1], context.lineWidth / 2, 0, Math.PI * 2);
          context.fill();
        }
      } else if (draw.op === "drawline" && numbers.length >= 4) {
        context.beginPath();
        context.moveTo(numbers[0], numbers[1]);
        for (let index = 2; index + 1 < numbers.length; index += 2) {
          context.lineTo(numbers[index], numbers[index + 1]);
        }
        context.stroke();
      } else if (draw.op === "drawrect") {
        for (let index = 0; index + 3 < numbers.length; index += 4) {
          const [x, y, w, h] = numbers.slice(index, index + 4);
          context.beginPath();
          if (switches.includes("e")) context.ellipse(x + w / 2, y + h / 2, Math.abs(w / 2), Math.abs(h / 2), 0, 0, Math.PI * 2);
          else context.rect(x, y, w, h);
          if (switches.includes("f")) context.fill();
          else context.stroke();
        }
      } else if (draw.op === "drawtext") {
        const x = Number(stroke) || 0;
        const y = Number(values[0]) || 0;
        context.font = "14px sans-serif";
        context.fillText(values.slice(1).join(" "), x, y);
      }
    }
  }, [drawing]);

  return <canvas ref={canvasRef} className="picture-window" aria-label={`${buffer.name} picture`} />;
}
