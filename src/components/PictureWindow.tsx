import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { Buffer } from "../state/store";
import { useStore } from "../state/store";
import { api, type PopupItem } from "../lib/api";

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
  const lastMouseEventRef = useRef(0);
  const drawing = buffer.windowDrawing ?? [];
  const server = useStore((state) => state.servers[buffer.serverId]);
  const [events, setEvents] = useState<Record<string, PopupItem>>({});

  useEffect(() => {
    api
      .scriptPopups(buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "", buffer.name, "")
      .then((items) => {
        const mapped: Record<string, PopupItem> = {};
        for (const item of items) {
          const event = item.label.trim().toLowerCase();
          if (["mouse", "sclick", "dclick", "uclick", "rclick", "lbclick", "leave", "drop"].includes(event)) {
            mapped[event] = item;
          }
        }
        setEvents(mapped);
      })
      .catch(() => setEvents({}));
  }, [buffer.name, buffer.serverId, server?.name, server?.nick]);

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
      } else if (draw.op === "drawfill" && numbers.length >= 2) {
        const x = Math.trunc(numbers[0]);
        const y = Math.trunc(numbers[1]);
        if (x < 0 || y < 0 || x >= width || y >= height) continue;
        const image = context.getImageData(0, 0, width, height);
        const start = (y * width + x) * 4;
        const target = image.data.slice(start, start + 4);
        context.fillStyle = color(colour);
        context.fillRect(0, 0, 1, 1);
        const replacement = context.getImageData(0, 0, 1, 1).data;
        context.putImageData(image, 0, 0);
        if (target.every((value, index) => value === replacement[index])) continue;
        const stack = [x, y];
        while (stack.length) {
          const py = stack.pop()!;
          const px = stack.pop()!;
          if (px < 0 || py < 0 || px >= width || py >= height) continue;
          const offset = (py * width + px) * 4;
          if (!target.every((value, index) => image.data[offset + index] === value)) continue;
          image.data.set(replacement, offset);
          stack.push(px - 1, py, px + 1, py, px, py - 1, px, py + 1);
        }
        context.putImageData(image, 0, 0);
      } else if (draw.op === "drawreplace") {
        const from = color(colour);
        const to = color(stroke);
        context.fillStyle = from;
        context.fillRect(0, 0, 1, 1);
        const fromPixel = context.getImageData(0, 0, 1, 1).data;
        context.fillStyle = to;
        context.fillRect(0, 0, 1, 1);
        const toPixel = context.getImageData(0, 0, 1, 1).data;
        const image = context.getImageData(0, 0, width, height);
        for (let offset = 0; offset < image.data.length; offset += 4) {
          if (fromPixel.every((value, index) => image.data[offset + index] === value)) {
            image.data.set(toPixel, offset);
          }
        }
        context.putImageData(image, 0, 0);
      }
    }
  }, [drawing]);

  const runEvent = (name: string, event: ReactMouseEvent<HTMLCanvasElement>) => {
    const item = events[name];
    const canvas = canvasRef.current;
    if (!item || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = Math.round((event.clientX - rect.left) * canvas.width / Math.max(1, rect.width));
    const y = Math.round((event.clientY - rect.top) * canvas.height / Math.max(1, rect.height));
    api.scriptWindowMouse(
      buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "",
      item.command, item.source ?? "", x, y
    ).catch(() => {});
  };

  return (
    <canvas
      ref={canvasRef}
      className="picture-window"
      aria-label={`${buffer.name} picture`}
      onMouseMove={(event) => {
        if (event.timeStamp - lastMouseEventRef.current >= 50) {
          lastMouseEventRef.current = event.timeStamp;
          runEvent("mouse", event);
        }
      }}
      onClick={(event) => { runEvent("sclick", event); runEvent("lbclick", event); }}
      onDoubleClick={(event) => runEvent("dclick", event)}
      onMouseUp={(event) => runEvent("uclick", event)}
      onContextMenu={(event) => { event.preventDefault(); runEvent("rclick", event); }}
      onMouseLeave={(event) => runEvent("leave", event)}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => { event.preventDefault(); runEvent("drop", event); }}
    />
  );
}
