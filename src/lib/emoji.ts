// Minimal :shortcode: -> emoji replacement for outgoing messages.
const MAP: Record<string, string> = {
  ":smile:": "😄",
  ":grin:": "😁",
  ":laughing:": "😆",
  ":joy:": "😂",
  ":wink:": "😉",
  ":blush:": "😊",
  ":heart:": "❤️",
  ":thumbsup:": "👍",
  ":+1:": "👍",
  ":thumbsdown:": "👎",
  ":-1:": "👎",
  ":fire:": "🔥",
  ":tada:": "🎉",
  ":rocket:": "🚀",
  ":eyes:": "👀",
  ":thinking:": "🤔",
  ":shrug:": "🤷",
  ":wave:": "👋",
  ":ok_hand:": "👌",
  ":100:": "💯",
  ":poop:": "💩",
  ":sob:": "😭",
  ":cry:": "😢",
  ":angry:": "😠",
  ":cool:": "😎",
  ":party:": "🥳",
  ":coffee:": "☕",
  ":beer:": "🍺",
  ":pizza:": "🍕",
  ":check:": "✅",
  ":x:": "❌",
  ":warning:": "⚠️",
  ":star:": "⭐",
  ":sparkles:": "✨",
  // faces & gestures
  ":slight_smile:": "🙂",
  ":upside_down:": "🙃",
  ":sweat_smile:": "😅",
  ":rofl:": "🤣",
  ":melting:": "🫠",
  ":smirk:": "😏",
  ":kissing_heart:": "😘",
  ":heart_eyes:": "😍",
  ":star_struck:": "🤩",
  ":hugging:": "🤗",
  ":zany:": "🤪",
  ":nerd:": "🤓",
  ":sunglasses:": "😎",
  ":monocle:": "🧐",
  ":unamused:": "😒",
  ":pensive:": "😔",
  ":relieved:": "😌",
  ":sleepy:": "😪",
  ":sleeping:": "😴",
  ":yawn:": "🥱",
  ":mask:": "😷",
  ":woozy:": "🥴",
  ":dizzy:": "😵",
  ":exploding_head:": "🤯",
  ":cowboy:": "🤠",
  ":partying:": "🥳",
  ":disguise:": "🥸",
  ":worried:": "😟",
  ":frowning:": "🙁",
  ":astonished:": "😲",
  ":flushed:": "😳",
  ":pleading:": "🥺",
  ":fearful:": "😨",
  ":cold_sweat:": "😰",
  ":scream:": "😱",
  ":confounded:": "😖",
  ":weary:": "😩",
  ":triumph:": "😤",
  ":rage:": "😡",
  ":swearing:": "🤬",
  ":smiling_devil:": "😈",
  ":imp:": "👿",
  ":skull:": "💀",
  ":clown:": "🤡",
  ":ghost:": "👻",
  ":alien:": "👽",
  ":robot:": "🤖",
  ":sneeze:": "🤧",
  ":vomit:": "🤮",
  ":nauseated:": "🤢",
  ":hot:": "🥵",
  ":cold:": "🥶",
  ":money_face:": "🤑",
  ":zipper_mouth:": "🤐",
  ":raised_eyebrow:": "🤨",
  ":neutral:": "😐",
  ":expressionless:": "😑",
  ":rolling_eyes:": "🙄",
  ":grimace:": "😬",
  ":lying:": "🤥",
  ":drool:": "🤤",
  ":salute:": "🫡",
  ":raising_hands:": "🙌",
  ":clap:": "👏",
  ":pray:": "🙏",
  ":handshake:": "🤝",
  ":muscle:": "💪",
  ":point_up:": "☝️",
  ":point_down:": "👇",
  ":point_left:": "👈",
  ":point_right:": "👉",
  ":crossed_fingers:": "🤞",
  ":vulcan:": "🖖",
  ":metal:": "🤘",
  ":call_me:": "🤙",
  ":fist:": "✊",
  ":punch:": "👊",
  ":v:": "✌️",
  ":writing:": "✍️",
  ":nail_care:": "💅",
  ":selfie:": "🤳",
  // hearts & symbols
  ":broken_heart:": "💔",
  ":two_hearts:": "💕",
  ":sparkling_heart:": "💖",
  ":heartpulse:": "💗",
  ":blue_heart:": "💙",
  ":green_heart:": "💚",
  ":yellow_heart:": "💛",
  ":purple_heart:": "💜",
  ":black_heart:": "🖤",
  ":white_heart:": "🤍",
  ":orange_heart:": "🧡",
  ":kiss:": "💋",
  ":dizzy_symbol:": "💫",
  ":boom:": "💥",
  ":sweat_drops:": "💦",
  ":zzz:": "💤",
  ":bulb:": "💡",
  ":anger:": "💢",
  ":question:": "❓",
  ":exclamation:": "❗",
  ":heavy_check:": "✔️",
  ":no_entry:": "⛔",
  ":recycle:": "♻️",
  ":hourglass:": "⌛",
  ":bell:": "🔔",
  ":lock:": "🔒",
  ":key:": "🔑",
  ":mag:": "🔍",
  ":link:": "🔗",
  // animals & nature
  ":dog:": "🐶",
  ":cat:": "🐱",
  ":mouse:": "🐭",
  ":fox:": "🦊",
  ":bear:": "🐻",
  ":panda:": "🐼",
  ":lion:": "🦁",
  ":tiger:": "🐯",
  ":unicorn:": "🦄",
  ":monkey:": "🐵",
  ":penguin:": "🐧",
  ":chicken:": "🐔",
  ":frog:": "🐸",
  ":snake:": "🐍",
  ":turtle:": "🐢",
  ":whale:": "🐳",
  ":dolphin:": "🐬",
  ":octopus:": "🐙",
  ":butterfly:": "🦋",
  ":bug:": "🐛",
  ":bee:": "🐝",
  ":snail:": "🐌",
  ":flower:": "🌸",
  ":rose:": "🌹",
  ":sunflower:": "🌻",
  ":seedling:": "🌱",
  ":tree:": "🌳",
  ":cactus:": "🌵",
  ":four_leaf_clover:": "🍀",
  ":sun:": "☀️",
  ":moon:": "🌙",
  ":rainbow:": "🌈",
  ":cloud:": "☁️",
  ":snowflake:": "❄️",
  ":zap:": "⚡",
  ":droplet:": "💧",
  ":ocean:": "🌊",
  ":earth:": "🌍",
  // food & drink
  ":apple:": "🍎",
  ":banana:": "🍌",
  ":strawberry:": "🍓",
  ":watermelon:": "🍉",
  ":grapes:": "🍇",
  ":cherries:": "🍒",
  ":peach:": "🍑",
  ":hamburger:": "🍔",
  ":fries:": "🍟",
  ":hotdog:": "🌭",
  ":taco:": "🌮",
  ":popcorn:": "🍿",
  ":doughnut:": "🍩",
  ":cookie:": "🍪",
  ":cake:": "🍰",
  ":birthday:": "🎂",
  ":icecream:": "🍦",
  ":chocolate:": "🍫",
  ":candy:": "🍬",
  ":wine:": "🍷",
  ":cocktail:": "🍸",
  ":tropical_drink:": "🍹",
  ":champagne:": "🍾",
  ":tea:": "🍵",
  // activities & objects
  ":soccer:": "⚽",
  ":basketball:": "🏀",
  ":football:": "🏈",
  ":baseball:": "⚾",
  ":tennis:": "🎾",
  ":trophy:": "🏆",
  ":medal:": "🏅",
  ":dart:": "🎯",
  ":game:": "🎮",
  ":dice:": "🎲",
  ":guitar:": "🎸",
  ":microphone:": "🎤",
  ":headphones:": "🎧",
  ":musical_note:": "🎵",
  ":notes:": "🎶",
  ":art:": "🎨",
  ":clapper:": "🎬",
  ":camera:": "📷",
  ":phone:": "📱",
  ":computer:": "💻",
  ":tv:": "📺",
  ":money:": "💰",
  ":dollar:": "💵",
  ":gift:": "🎁",
  ":balloon:": "🎈",
  ":gem:": "💎",
  ":crown:": "👑",
  ":umbrella:": "☂️",
  ":hammer:": "🔨",
  ":wrench:": "🔧",
  ":gear:": "⚙️",
  ":battery:": "🔋",
  ":mail:": "📧",
  ":pencil:": "📝",
  ":book:": "📖",
  ":clipboard:": "📋",
  ":calendar:": "📅",
  ":pushpin:": "📌",
  ":scissors:": "✂️",
  ":paperclip:": "📎",
  // travel & places
  ":car:": "🚗",
  ":taxi:": "🚕",
  ":bus:": "🚌",
  ":police_car:": "🚓",
  ":ambulance:": "🚑",
  ":fire_engine:": "🚒",
  ":bike:": "🚲",
  ":motorcycle:": "🏍️",
  ":airplane:": "✈️",
  ":helicopter:": "🚁",
  ":ship:": "🚢",
  ":anchor:": "⚓",
  ":rocket_ship:": "🚀",
  ":house:": "🏠",
  ":office:": "🏢",
  ":hospital:": "🏥",
  ":school:": "🏫",
  ":castle:": "🏰",
  ":statue_of_liberty:": "🗽",
  ":mountain:": "⛰️",
  ":beach:": "🏖️",
  ":desert:": "🏜️",
  ":volcano:": "🌋",
  ":camping:": "🏕️",
  ":sunrise:": "🌅",
  ":night:": "🌃",
  // flags & misc
  ":checkered_flag:": "🏁",
  ":triangular_flag:": "🚩",
  ":rainbow_flag:": "🏳️‍🌈",
  ":white_flag:": "🏳️",
  ":black_flag:": "🏴",
  ":no_good:": "🙅",
  ":ok_woman:": "🙆",
  ":shrug_person:": "🤷",
  ":facepalm:": "🤦",
  ":tipping_hand:": "💁",
  ":dancer:": "💃",
  ":dancing_man:": "🕺",
  ":running:": "🏃",
  ":walking:": "🚶",
  ":swimmer:": "🏊",
  ":surfer:": "🏄",
  ":weight_lifter:": "🏋️",
  ":bath:": "🛀",
};

import { useSettings } from "../state/settings";

/** True if a custom-emoji value should render as an inline image. */
function isImage(v: string): boolean {
  return /^(https?:|data:)/i.test(v);
}

export interface PickerEmoji {
  title: string;
  /** Text/unicode glyph to show (when not an image). */
  glyph?: string;
  /** Image URL to show (custom image emoji). */
  img?: string;
  /** What to insert into the input when clicked. */
  insert: string;
}

/** The set of emoji shown in the input-bar picker: the built-in glyphs plus any
 *  custom emoji from settings. */
export function emojiPicker(): PickerEmoji[] {
  const out: PickerEmoji[] = [];
  const seen = new Set<string>();
  for (const [code, glyph] of Object.entries(MAP)) {
    if (seen.has(glyph)) continue;
    seen.add(glyph);
    out.push({ title: code, glyph, insert: glyph });
  }
  for (const [code, v] of Object.entries(useSettings.getState().customEmoji ?? {})) {
    if (isImage(v)) out.push({ title: code, img: v, insert: code });
    else out.push({ title: code, glyph: v, insert: code });
  }
  return out;
}

function customEmoji(): Record<string, string> {
  const raw = useSettings.getState().customEmoji ?? {};
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw)) out[k.toLowerCase()] = v;
  return out;
}

/** Custom emoji whose value is an image URL, rendered inline at display time. */
export function imageEmoji(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(customEmoji())) {
    if (isImage(v)) out[k] = v;
  }
  return out;
}

/** Replaces known :shortcodes: in `text` with emoji (built-in + custom text);
 *  image-valued custom emoji are left as `:code:` and rendered when displayed. */
export function expandEmoji(text: string): string {
  const custom = customEmoji();
  return text.replace(/:[a-z0-9_+-]+:/gi, (m) => {
    const key = m.toLowerCase();
    const c = custom[key];
    if (c !== undefined) return isImage(c) ? m : c;
    return MAP[key] ?? m;
  });
}
