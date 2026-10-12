/** DOM-only confetti burst (Web Animations API), excluded from coverage. */

const COLORS = ["#ff5fa2", "#7c5cff", "#4ade80", "#fbbf24", "#67e8f9"];
const PIECES = 28;
const DURATION_MS = 900;

export function burstConfetti(
  container: HTMLElement,
  x: number,
  y: number,
): void {
  for (let i = 0; i < PIECES; i++) {
    const piece = document.createElement("span");
    piece.className = "confetti-piece";
    const angle = (Math.PI * 2 * i) / PIECES + Math.random() * 0.6;
    const distance = 40 + Math.random() * 70;
    const dx = Math.cos(angle) * distance;
    const dy = Math.sin(angle) * distance - 30;
    piece.style.left = `${x}px`;
    piece.style.top = `${y}px`;
    piece.style.background = COLORS[i % COLORS.length] ?? "#ff5fa2";
    piece.style.width = `${6 + Math.random() * 5}px`;
    piece.style.height = `${8 + Math.random() * 6}px`;
    container.append(piece);
    piece
      .animate(
        [
          { transform: "translate(-50%, -50%) rotate(0deg)", opacity: 1 },
          {
            transform: `translate(calc(-50% + ${dx}px), calc(-50% + ${dy}px)) rotate(${(Math.random() - 0.5) * 540}deg)`,
            opacity: 0,
          },
        ],
        { duration: DURATION_MS, easing: "cubic-bezier(.16,.8,.32,1)" },
      )
      .addEventListener("finish", () => piece.remove());
  }
}
