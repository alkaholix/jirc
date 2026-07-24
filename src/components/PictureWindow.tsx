import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { Buffer } from "../state/store";
import { useStore } from "../state/store";
import { api, type PopupItem } from "../lib/api";

const MIRC_COLORS = [
  "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000",
  "#9c009c", "#fc7f00", "#ffff00", "#00fc00", "#009393", "#00ffff",
  "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
];
const imageCache = new Map<string, Promise<HTMLImageElement>>();

function color(value = ""): string {
  const numeric = Number(value);
  if (Number.isInteger(numeric) && numeric >= 0 && numeric < MIRC_COLORS.length) return MIRC_COLORS[numeric];
  if (Number.isInteger(numeric) && numeric >= 0) {
    return `rgb(${numeric & 0xff},${(numeric >> 8) & 0xff},${(numeric >> 16) & 0xff})`;
  }
  return value || "#ffffff";
}

function pixelFor(context: CanvasRenderingContext2D, value: string): Uint8ClampedArray {
  const previous = context.fillStyle;
  context.fillStyle = color(value);
  context.fillRect(0, 0, 1, 1);
  const pixel = context.getImageData(0, 0, 1, 1).data.slice();
  context.fillStyle = previous;
  return pixel;
}

function samePixel(data: Uint8ClampedArray, offset: number, pixel: Uint8ClampedArray): boolean {
  return pixel.every((value, index) => data[offset + index] === value);
}

function loadImage(filename: string): Promise<HTMLImageElement> {
  let pending = imageCache.get(filename);
  if (!pending) {
    const source = filename.startsWith("data:") ? Promise.resolve(filename) : api.scriptPictureRead(filename);
    pending = source.then((source) => new Promise<HTMLImageElement>((resolve, reject) => {
      const image = new Image();
      image.onload = () => resolve(image);
      image.onerror = () => reject(new Error(`Unable to load ${filename}`));
      image.src = source;
    }));
    imageCache.set(filename, pending);
  }
  return pending;
}

function bmpDataUrl(canvas: HTMLCanvasElement): string {
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return "";
  const { width, height } = canvas;
  const rgba = context.getImageData(0, 0, width, height).data;
  const rowSize = Math.ceil(width * 3 / 4) * 4;
  const bytes = new Uint8Array(54 + rowSize * height);
  const view = new DataView(bytes.buffer);
  bytes.set([0x42, 0x4d]);
  view.setUint32(2, bytes.length, true);
  view.setUint32(10, 54, true);
  view.setUint32(14, 40, true);
  view.setInt32(18, width, true);
  view.setInt32(22, height, true);
  view.setUint16(26, 1, true);
  view.setUint16(28, 24, true);
  view.setUint32(34, rowSize * height, true);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const source = ((height - 1 - y) * width + x) * 4;
      const destination = 54 + y * rowSize + x * 3;
      bytes[destination] = rgba[source + 2];
      bytes[destination + 1] = rgba[source + 1];
      bytes[destination + 2] = rgba[source];
    }
  }
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return `data:image/bmp;base64,${btoa(binary)}`;
}

function bytesToBase64(bytes: Uint8ClampedArray): string {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

function visibleOperations(drawing: Buffer["windowDrawing"]): NonNullable<Buffer["windowDrawing"]> {
  const operations = drawing ?? [];
  let end = operations.length;
  while (end > 0 && operations[end - 1].args[0]?.includes("n")) end -= 1;
  return operations.slice(0, end);
}

async function renderOperations(
  canvas: HTMLCanvasElement,
  drawing: NonNullable<Buffer["windowDrawing"]>,
  sourceLookup: (name: string) => Buffer | undefined,
): Promise<void> {
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return;
  const size = [...drawing].reverse().find((item) => item.op === "drawsize");
  const width = Math.min(8192, Math.max(1, Number(size?.args[1]) || canvas.clientWidth || canvas.width || 640));
  const height = Math.min(8192, Math.max(1, Number(size?.args[2]) || canvas.clientHeight || canvas.height || 400));
  canvas.width = width;
  canvas.height = height;

  for (const draw of drawing) {
    const switches = draw.args[0] ?? "";
    if (draw.op === "drawsize" || draw.args.length === 1) continue;
    context.save();
    if (switches.includes("i")) context.globalCompositeOperation = "difference";
    if (switches.includes("m")) context.imageSmoothingQuality = "high";

    if (draw.op === "drawdot" || draw.op === "drawline" || draw.op === "drawrect") {
      const [colour, stroke, ...raw] = draw.args.slice(1);
      const numbers = raw.map(Number);
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
        for (let index = 2; index + 1 < numbers.length; index += 2) context.lineTo(numbers[index], numbers[index + 1]);
        context.stroke();
      } else {
        const rounded = switches.includes("d") && numbers.length >= 6;
        const coordinateEnd = rounded ? numbers.length - 2 : numbers.length;
        for (let index = 0; index + 3 < coordinateEnd; index += 4) {
          const [x, y, w, h] = numbers.slice(index, index + 4);
          context.beginPath();
          if (switches.includes("e")) {
            context.ellipse(x + w / 2, y + h / 2, Math.abs(w / 2), Math.abs(h / 2), 0, 0, Math.PI * 2);
          } else if (rounded) {
            context.roundRect(x, y, w, h, [Math.abs(numbers.at(-2)!) / 2, Math.abs(numbers.at(-1)!) / 2]);
          } else {
            context.rect(x, y, w, h);
          }
          if (switches.includes("f")) context.fill(); else context.stroke();
        }
      }
    } else if (draw.op === "drawtext") {
      let index = 1;
      const foreground = draw.args[index++] ?? "1";
      const background = switches.includes("b") ? draw.args[index++] : undefined;
      let font = "sans-serif";
      let fontSize = 14;
      if (!Number.isFinite(Number(draw.args[index])) && Number.isFinite(Number(draw.args[index + 1]))) {
        font = draw.args[index++];
        fontSize = Math.abs(Number(draw.args[index++])) || 14;
      }
      const x = Number(draw.args[index++]) || 0;
      const y = Number(draw.args[index++]) || 0;
      let clipWidth: number | undefined;
      let clipHeight: number | undefined;
      if (switches.includes("c")) {
        clipWidth = Number(draw.args[index++]) || 0;
        clipHeight = Number(draw.args[index++]) || 0;
      }
      const text = draw.args.slice(index).join(" ").replaceAll("\\t", switches.includes("d") ? " " : "\t");
      context.font = `${switches.includes("o") ? "bold " : ""}${fontSize}px ${font}`;
      context.textBaseline = "top";
      if (clipWidth !== undefined && clipHeight !== undefined) {
        context.beginPath();
        context.rect(x, y, clipWidth, clipHeight);
        context.clip();
      }
      if (background) {
        const metrics = context.measureText(text);
        context.fillStyle = color(background);
        context.fillRect(x, y, clipWidth ?? metrics.width, clipHeight ?? fontSize * 1.25);
      }
      context.fillStyle = color(foreground);
      context.fillText(text, x, y);
    } else if (draw.op === "drawfill") {
      const [fillColour, boundaryColour, xRaw, yRaw, patternFile] = draw.args.slice(1);
      const x = Math.trunc(Number(xRaw));
      const y = Math.trunc(Number(yRaw));
      if (x >= 0 && y >= 0 && x < width && y < height) {
        const image = context.getImageData(0, 0, width, height);
        const startPixel = image.data.slice((y * width + x) * 4, (y * width + x) * 4 + 4);
        const boundary = pixelFor(context, boundaryColour);
        context.putImageData(image, 0, 0);
        const fill = pixelFor(context, fillColour);
        context.putImageData(image, 0, 0);
        const visited = new Uint8Array(width * height);
        const points = [x, y];
        const filled: number[] = [];
        while (points.length) {
          const py = points.pop()!;
          const px = points.pop()!;
          if (px < 0 || py < 0 || px >= width || py >= height) continue;
          const point = py * width + px;
          if (visited[point]) continue;
          visited[point] = 1;
          const offset = point * 4;
          const mayFill = switches.includes("s")
            ? samePixel(image.data, offset, boundary)
            : !samePixel(image.data, offset, boundary);
          if (!mayFill) continue;
          if (!switches.includes("s") && samePixel(startPixel, 0, boundary)) continue;
          image.data.set(fill, offset);
          filled.push(px, py);
          points.push(px - 1, py, px + 1, py, px, py - 1, px, py + 1);
        }
        context.putImageData(image, 0, 0);
        if (patternFile && filled.length) {
          try {
            const pattern = context.createPattern(await loadImage(patternFile), "repeat");
            if (pattern) {
              context.fillStyle = pattern;
              for (let point = 0; point < filled.length; point += 2) context.fillRect(filled[point], filled[point + 1], 1, 1);
            }
          } catch { /* An unreadable optional pattern leaves the solid fill. */ }
        }
      }
    } else if (draw.op === "drawreplace") {
      const [from, to, xRaw, yRaw, wRaw, hRaw] = draw.args.slice(1);
      const x = Number(xRaw) || 0;
      const y = Number(yRaw) || 0;
      const w = Number(wRaw) || width;
      const h = Number(hRaw) || height;
      const image = context.getImageData(x, y, Math.min(w, width - x), Math.min(h, height - y));
      const fromPixel = pixelFor(context, from);
      const toPixel = pixelFor(context, to);
      for (let offset = 0; offset < image.data.length; offset += 4) {
        if (samePixel(image.data, offset, fromPixel)) image.data.set(toPixel, offset);
      }
      context.putImageData(image, x, y);
    } else if (draw.op === "drawscroll") {
      const values = draw.args.slice(1).map(Number);
      for (let index = 0; index + 5 < values.length; index += 6) {
        const [dx, dy, x, y, w, h] = values.slice(index, index + 6);
        const snapshot = context.getImageData(x, y, w, h);
        context.clearRect(x, y, w, h);
        context.putImageData(snapshot, x + dx, y + dy);
      }
    } else if (draw.op === "drawcopy") {
      const sourceName = draw.args[1];
      const destinationIndex = draw.args.findIndex((value, index) => index > 1 && value.startsWith("@"));
      if (destinationIndex > 0) {
        let index = 2;
        const transparent = switches.includes("t") ? draw.args[index++] : undefined;
        const [sx, sy, sw, sh] = draw.args.slice(index, index + 4).map(Number);
        const [dx, dy, dw = sw, dh = sh] = draw.args.slice(destinationIndex + 1).map(Number);
        let sourceCanvas = canvas;
        if (sourceName.toLowerCase() !== draw.args[destinationIndex].toLowerCase()) {
          const source = sourceLookup(sourceName);
          if (source) {
            sourceCanvas = document.createElement("canvas");
            await renderOperations(sourceCanvas, visibleOperations(source.windowDrawing), sourceLookup);
          }
        }
        const copy = document.createElement("canvas");
        copy.width = sw;
        copy.height = sh;
        copy.getContext("2d")?.drawImage(sourceCanvas, sx, sy, sw, sh, 0, 0, sw, sh);
        if (transparent) {
          const copyContext = copy.getContext("2d", { willReadFrequently: true });
          if (copyContext) {
            const image = copyContext.getImageData(0, 0, sw, sh);
            const transparentPixel = pixelFor(copyContext, transparent);
            for (let offset = 0; offset < image.data.length; offset += 4) {
              if (samePixel(image.data, offset, transparentPixel)) image.data[offset + 3] = 0;
            }
            copyContext.putImageData(image, 0, 0);
          }
        }
        context.drawImage(copy, dx, dy, dw, dh);
      }
    } else if (draw.op === "drawpic") {
      let index = 1;
      const transparent = switches.includes("t") ? draw.args[index++] : undefined;
      const x = Number(draw.args[index++]) || 0;
      const y = Number(draw.args[index++]) || 0;
      let destinationWidth: number | undefined;
      let destinationHeight: number | undefined;
      if (switches.includes("s")) {
        destinationWidth = Number(draw.args[index++]);
        destinationHeight = Number(draw.args[index++]);
      }
      let sourceRect: number[] | undefined;
      const tailCount = (switches.includes("o") ? 1 : 0) + (switches.includes("f") ? 1 : 0) + 1;
      if (draw.args.length - index >= tailCount + 4) sourceRect = draw.args.slice(index, index += 4).map(Number);
      if (switches.includes("o")) index += 1;
      if (switches.includes("f")) index += 1;
      const filename = draw.args[index];
      if (filename) {
        try {
          const loaded = await loadImage(filename);
          let image: CanvasImageSource = loaded;
          if (transparent) {
            const prepared = document.createElement("canvas");
            prepared.width = loaded.width;
            prepared.height = loaded.height;
            const preparedContext = prepared.getContext("2d", { willReadFrequently: true });
            if (preparedContext) {
              preparedContext.drawImage(loaded, 0, 0);
              const pixels = preparedContext.getImageData(0, 0, prepared.width, prepared.height);
              const transparentPixel = pixelFor(preparedContext, transparent);
              for (let offset = 0; offset < pixels.data.length; offset += 4) {
                if (samePixel(pixels.data, offset, transparentPixel)) pixels.data[offset + 3] = 0;
              }
              preparedContext.putImageData(pixels, 0, 0);
              image = prepared;
            }
          }
          if (switches.includes("l")) {
            const pattern = context.createPattern(image, "repeat");
            if (pattern) { context.fillStyle = pattern; context.fillRect(x, y, destinationWidth ?? width - x, destinationHeight ?? height - y); }
          } else if (sourceRect) {
            const [sx, sy, sw, sh] = sourceRect;
            context.drawImage(image, sx, sy, sw, sh, x, y, destinationWidth ?? sw, destinationHeight ?? sh);
          } else {
            context.drawImage(image, x, y, destinationWidth ?? loaded.width, destinationHeight ?? loaded.height);
          }
        } catch { /* mIRC also leaves the canvas unchanged when loading fails. */ }
      }
    } else if (draw.op === "drawrot") {
      let index = 1;
      const background = switches.includes("b") ? draw.args[index++] : undefined;
      const angle = (Number(draw.args[index++]) || 0) * Math.PI / 180;
      const [x = 0, y = 0, w = width, h = height] = draw.args.slice(index).map(Number);
      const source = document.createElement("canvas");
      source.width = w;
      source.height = h;
      source.getContext("2d")?.drawImage(canvas, x, y, w, h, 0, 0, w, h);
      context.save();
      if (switches.includes("p")) { context.beginPath(); context.rect(x, y, w, h); context.clip(); }
      if (background) { context.fillStyle = color(background); context.fillRect(x, y, w, h); } else context.clearRect(x, y, w, h);
      context.translate(x + w / 2, y + h / 2);
      context.rotate(angle);
      if (switches.includes("f")) context.drawImage(source, -w / 2, -h / 2, w, h);
      else context.drawImage(source, -w / 2, -h / 2);
      context.restore();
    } else if (draw.op === "drawsave") {
      let index = 1;
      let output = canvas;
      if (switches.includes("a")) {
        const [x, y, w, h] = draw.args.slice(index, index += 4).map(Number);
        output = document.createElement("canvas");
        output.width = w;
        output.height = h;
        output.getContext("2d")?.drawImage(canvas, x, y, w, h, 0, 0, w, h);
      }
      const filename = draw.args[index];
      if (filename && switches.includes("v")) {
        const format = switches.match(/v([pgj])/i)?.[1]?.toLowerCase();
        const mime = format === "j" ? "image/jpeg" : "image/png";
        await api.scriptPictureBinvar(filename, output.toDataURL(mime));
      } else if (filename) {
        const mime = /\.jpe?g$/i.test(filename) ? "image/jpeg" : "image/png";
        const quality = Number(switches.match(/q(\d+)/i)?.[1] ?? 92) / 100;
        const data = /\.bmp$/i.test(filename) ? bmpDataUrl(output) : output.toDataURL(mime, quality);
        await api.scriptPictureSave(filename, data);
      }
    }
    context.restore();
  }
}

export function PictureWindow({ buffer }: { buffer: Buffer }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const lastMouseEventRef = useRef(0);
  const drawing = buffer.windowDrawing ?? [];
  const server = useStore((state) => state.servers[buffer.serverId]);
  const [events, setEvents] = useState<Record<string, PopupItem>>({});

  useEffect(() => {
    api.scriptPopups(buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "", buffer.name, "")
      .then((items) => {
        const mapped: Record<string, PopupItem> = {};
        for (const item of items) {
          const event = item.label.trim().toLowerCase();
          if (["mouse", "sclick", "dclick", "uclick", "rclick", "lbclick", "leave", "drop"].includes(event)) mapped[event] = item;
        }
        setEvents(mapped);
      })
      .catch(() => setEvents({}));
  }, [buffer.name, buffer.serverId, server?.name, server?.nick]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    let cancelled = false;
    const rendered = document.createElement("canvas");
    rendered.width = canvas.clientWidth || canvas.width || 640;
    rendered.height = canvas.clientHeight || canvas.height || 400;
    const lookup = (name: string) => Object.values(useStore.getState().buffers)
      .find((candidate) => candidate.serverId === buffer.serverId && candidate.name.toLowerCase() === name.toLowerCase());
    renderOperations(rendered, visibleOperations(drawing), lookup)
      .then(() => {
        if (cancelled) return;
        canvas.width = rendered.width;
        canvas.height = rendered.height;
        canvas.getContext("2d")?.drawImage(rendered, 0, 0);
        const context = rendered.getContext("2d", { willReadFrequently: true });
        if (!context) return;
        const rgba = context.getImageData(0, 0, rendered.width, rendered.height).data;
        api.scriptPictureSnapshot(buffer.name, rendered.width, rendered.height, bytesToBase64(rgba)).catch(() => {});
      })
      .catch(() => {
        if (!cancelled) canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
      });
    return () => { cancelled = true; };
  }, [buffer.serverId, drawing]);

  const runEvent = (name: string, event: ReactMouseEvent<HTMLCanvasElement>) => {
    const item = events[name];
    const canvas = canvasRef.current;
    if (!item || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = Math.round((event.clientX - rect.left) * canvas.width / Math.max(1, rect.width));
    const y = Math.round((event.clientY - rect.top) * canvas.height / Math.max(1, rect.height));
    const key = (event.buttons & 1 ? 1 : 0) | (event.ctrlKey ? 2 : 0)
      | (event.shiftKey ? 4 : 0) | (event.altKey ? 8 : 0) | (event.buttons & 2 ? 16 : 0);
    api.scriptWindowMouse(
      buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "",
      item.command, item.source ?? "", x, y, 0, key
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
      onClick={(event) => runEvent("sclick", event)}
      onDoubleClick={(event) => runEvent("dclick", event)}
      onMouseUp={(event) => runEvent("uclick", event)}
      onContextMenu={(event) => { event.preventDefault(); runEvent("rclick", event); }}
      onMouseLeave={(event) => runEvent("leave", event)}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => { event.preventDefault(); runEvent("drop", event); }}
    />
  );
}
