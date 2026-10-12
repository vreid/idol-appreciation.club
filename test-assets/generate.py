#!/usr/bin/env python3
"""Generate the 100 fictional-idol test images via the NanoGPT Image API.

Stdlib only (urllib, json, base64). Reads NANO_GPT_API_KEY from ../.env
relative to this script. Writes idols/NN-slug.jpg and manifest.json.

Usage: python3 generate.py [--only NN]
"""
import base64
import json
import os
import struct
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
API = "https://api.nano-gpt.com/api/v1/images"
MODEL = "bytedance/seedream-v5.0-pro"
RESOLUTION = "1k"  # supported tiers per endpoints metadata: aspect ratios + "1k"/"2k"
HERO_RESOLUTION = "2k"  # occasional hero shots
HERO_NUMBERS = {"30", "59", "64", "65"}
BUDGET_TARGET_USD = 9.5  # hard lifetime NanoGPT cap for this asset set
PER_IMAGE_EST_USD = 0.10
HERO_EST_USD = 0.35      # conservative pre-flight estimate for 2k requests
MIN_BYTES = 30 * 1024

TAIL = ("professional photography, editorial quality, sharp focus, "
        "high detail, fictional idol, AI-generated fictional character, "
        "no real person")

CATEGORIES = [
    ("01", "solo-female-closeup", "3:4",
     "Photocard-style close-up portrait of a young Korean woman fictional idol, "
     "soft wavy hair, flawless dewy idol makeup, gentle confident smile, soft "
     "pastel lavender studio background, beauty lighting, 85mm lens, " + TAIL),
    ("02", "solo-male-stage", "16:9",
     "Full-body shot of a male Korean fictional idol mid-performance on a huge "
     "concert stage, stylish stage costume, dramatic spotlight beams and haze, "
     "singing into a headset mic, cinematic concert photography, 85mm lens, " + TAIL),
    ("03", "group-five", "16:9",
     "Five-member coed K-pop group of fictional idols lined up shoulder to "
     "shoulder on a music-show stage, coordinated black-and-white stage "
     "outfits, colorful LED backdrop, group promo photography, 50mm lens, " + TAIL),
    ("04", "lightstick-ocean", "16:9",
     "Concert crowd seen from the stage point of view at night, thousands of "
     "glowing pastel lightsticks waving in the dark like an ocean, distant "
     "silhouette of a fictional idol performing, cinematic wide arena "
     "photography, 24mm lens, " + TAIL),
    ("05", "dance-break", "3:4",
     "Dynamic dance-break freeze frame of a fictional K-pop idol mid-move, "
     "clothes and hair in motion, sweat and intensity, dramatic arena lighting "
     "with lens flare, high-speed sports photography look, " + TAIL),
    ("06", "airport-fashion", "3:4",
     "Candid paparazzi-style photo of a stylish female fictional idol walking "
     "through an airport terminal pulling a suitcase, oversized designer "
     "streetwear, face mask pulled down to her chin, flash photography "
     "aesthetic, editorial street style, " + TAIL),
    ("07", "rehearsal", "16:9",
     "Practice-room mirror rehearsal shot of a fictional idol group in casual "
     "sweats, late-night warm lighting, water bottles and phones on the floor, "
     "candid documentary photography, 35mm lens, " + TAIL),
    ("08", "music-show-win", "16:9",
     "A fictional idol group celebrating an encore win on a music-show stage, "
     "holding a shiny trophy while golden confetti rains down, joyful tears "
     "and laughter, bright stage lights, celebratory editorial photography, " + TAIL),
    ("09", "dark-concept", "3:4",
     "Cinematic dark-concept teaser shot of a fictional idol, moody black set "
     "with deep shadows and a single dramatic rim light, high-fashion avant "
     "garde styling, smoldering gaze, film-noir editorial photography, " + TAIL),
    ("10", "photobook-nature", "3:4",
     "Photobook concept photo of a female fictional idol standing in a "
     "wildflower field at golden hour, flowing white summer dress, warm "
     "backlit sun flare, dreamy soft focus background, editorial fashion "
     "photography, 85mm lens, " + TAIL),
    ("11", "fanmeet-heart", "1:1",
     "A cheerful female fictional idol making a finger-heart gesture toward "
     "the camera at a fan meeting, warm golden stage lights, soft bokeh crowd "
     "background, cute pastel stage outfit, heartwarming event photography, " + TAIL),
    ("12", "birthday-cafe", "1:1",
     "Fan-organized birthday cafe display for a fictional idol, pastel banners "
     "decorated with star symbols, a beautiful cream cake, photo wall with "
     "candid prints, cozy cafe interior, cozy lifestyle photography, " + TAIL),
    ("13", "variety-pose", "16:9",
     "Playful fictional idol striking a goofy variety-show pose on a bright "
     "colorful studio set, oversized novelty props, laughing expression, "
     "vibrant game-show lighting, energetic entertainment photography, " + TAIL),
    ("14", "album-flatlay", "1:1",
     "K-pop album unboxing flat lay photographed from directly above, generic "
     "pastel album sleeve with abstract star artwork, shiny photocard, CD with "
     "iridescent reflection, photocard sleeves and stickers on a clean desk, "
     "no readable text or logos, product photography, " + TAIL),
    ("15", "finale-fireworks", "16:9",
     "Wide shot of an outdoor festival stage finale at night, fictional idol "
     "group silhouetted on the main stage while huge colorful fireworks "
     "explode over a massive crowd holding lightsticks, epic cinematic concert "
     "photography, 24mm lens, " + TAIL),
    ("16", "beauty-rainbow", "1:1",
     "Extreme close-up beauty shot of a fictional idol's face, rainbow prism "
     "light refracting across flawless dewy skin, glitter freckles, glossy "
     "lips, iridescent highlighter, high-end beauty editorial photography, "
     "macro 100mm lens, " + TAIL),
    ("17", "solo-pastel-photocard", "3:4",
     "Photocard-style studio portrait of a young Korean woman fictional idol "
     "with sleek black hair and a pastel pink outfit, holding both hands near "
     "her cheeks in a cute pose, mint and lavender gradient backdrop, soft "
     "even beauty lighting, 85mm lens, " + TAIL),
    ("18", "solo-spring-blossom", "3:4",
     "Spring portrait of a young Korean woman fictional idol under blooming "
     "cherry blossom trees, petals drifting in the air, beige cardigan and "
     "pleated skirt, warm afternoon light, shallow depth of field, editorial "
     "seasonal photography, 85mm lens, " + TAIL),
    ("19", "solo-summer-beach", "3:4",
     "Summer beach portrait of a young Korean woman fictional idol in a white "
     "linen shirt and straw hat, ocean waves and bright blue sky behind her, "
     "wind in her hair, golden sunlight, vacation photobook aesthetic, 85mm "
     "lens, " + TAIL),
    ("20", "solo-autumn-park", "3:4",
     "Autumn portrait of a young Korean woman fictional idol in a camel "
     "trench coat and plaid scarf standing in a park full of golden ginkgo "
     "trees, fallen leaves swirling, cozy warm tones, editorial fall fashion "
     "photography, 85mm lens, " + TAIL),
    ("21", "solo-winter-snow", "3:4",
     "Winter portrait of a young Korean woman fictional idol in a long cream "
     "wool coat and knitted muffler, snowflakes falling gently around her, "
     "breath visible in cold air, soft dusk light over a snowy street, "
     "editorial winter photography, 85mm lens, " + TAIL),
    ("22", "solo-halloween-witch", "3:4",
     "Halloween concept portrait of a young Korean woman fictional idol in a "
     "playful cute witch costume with a small round hat, holding a carved "
     "pumpkin bucket, orange and purple party lights with gentle fog, festive "
     "studio photography, " + TAIL),
    ("23", "solo-christmas-knit", "3:4",
     "Christmas concept portrait of a young Korean woman fictional idol in a "
     "cozy red knitted sweater beside a decorated tree with warm fairy "
     "lights, holding a mug with both hands, soft bokeh, holiday editorial "
     "photography, 85mm lens, " + TAIL),
    ("24", "duo-unit-pastel", "3:4",
     "Two-member female fictional idol unit posing together for a pastel "
     "concept photo, matching mint and pink outfits, cheerful synchronized "
     "poses, clean studio backdrop with soft clouds, unit debut promo "
     "photography, 50mm lens, " + TAIL),
    ("25", "duo-unit-hiphop", "16:9",
     "Two-member female fictional idol hip-hop unit performing a synchronized "
     "move on an underground-style stage, black and silver streetwear, "
     "dramatic single spotlight, gritty urban concert photography, 35mm lens, "
     + TAIL),
    ("26", "trio-unit-street", "3:4",
     "Three-member female fictional idol unit posing on a pastel city "
     "crosswalk for a street-style concept photo, coordinated denim outfits, "
     "confident expressions, morning sunlight, fashion editorial photography, "
     "50mm lens, " + TAIL),
    ("27", "group-four-girls", "16:9",
     "Four-member girl group of fictional idols performing on a music-show "
     "stage in coordinated white and blue outfits, mirror-like LED floor "
     "reflections, sharp synchronized choreography, dynamic concert "
     "photography, 35mm lens, " + TAIL),
    ("28", "group-five-girls", "16:9",
     "Five-member girl group of fictional idols striking an ending pose on a "
     "brightly lit music-show stage, pastel gradient LED backdrop, "
     "coordinated sparkling outfits, confetti in the air, energetic stage "
     "photography, 35mm lens, " + TAIL),
    ("29", "group-six-girls", "16:9",
     "Six-member girl group of fictional idols in a V formation on a dark "
     "arena stage, crimson and gold stage lighting, dramatic synchronized "
     "choreography with flowing ribbons, epic concert photography, 35mm lens, "
     + TAIL),
    ("30", "group-seven-girls", "16:9",
     "Epic wide shot of a seven-member girl group of fictional idols filling "
     "a massive award-show stage, elaborate silver-and-white stage costumes, "
     "towering LED columns and laser beams, powerful synchronized "
     "choreography, cinematic wide concert photography, 24mm lens, " + TAIL),
    ("31", "girlgroup-dance-break", "16:9",
     "Girl group of fictional idols frozen mid dance-break on a music-show "
     "stage, five dancers in sharp synchronized poses, dust and haze kicking "
     "up under strobe lights, high-speed concert photography, 70mm lens, "
     + TAIL),
    ("32", "girlgroup-music-win", "16:9",
     "Girl group of fictional idols celebrating a music-show win, members "
     "hugging and laughing around a shiny trophy while golden confetti rains "
     "down, tears of joy, bright celebratory stage lighting, editorial event "
     "photography, 35mm lens, " + TAIL),
    ("33", "encore-music-show", "16:9",
     "Encore stage in the style of a Korean music show, girl group of "
     "fictional idols waving tearfully to cameras holding microphones, "
     "glittering trophy on a pedestal in front of them, warm golden stage "
     "light, celebratory broadcast photography, 50mm lens, " + TAIL),
    ("34", "comeback-led-stage", "16:9",
     "Comeback debut stage of a girl group of fictional idols on a futuristic "
     "music-show set, towering curved LED screens pulsing neon geometry, "
     "metallic stage outfits, dramatic fog and backlight, cinematic stage "
     "photography, 35mm lens, " + TAIL),
    ("35", "solo-ballad-stage", "16:9",
     "Female fictional idol singing a ballad alone on a huge dark stage, "
     "single warm spotlight from above, long flowing dress catching the "
     "light, emotional closed-eye expression, cinematic concert photography, "
     "85mm lens, " + TAIL),
    ("36", "solo-dance-break", "3:4",
     "Dynamic solo dance-break shot of a female fictional idol mid-spin, red "
     "stage outfit with flowing fabric, hair whipping through dramatic rim "
     "light, motion blur background, high-speed performance photography, "
     + TAIL),
    ("37", "solo-male-ballad", "16:9",
     "Male fictional idol in a black suit singing an emotional ballad at a "
     "grand piano on a candle-lit stage, soft golden key light, gentle haze, "
     "intimate concert photography, 85mm lens, " + TAIL),
    ("38", "coed-festival-stage", "16:9",
     "Coed group of fictional idols performing at an outdoor university "
     "festival at dusk, mixed male and female members in casual stage "
     "outfits, cheering crowd with phone flashlights, warm festival lighting, "
     "documentary concert photography, 35mm lens, " + TAIL),
    ("39", "variety-game-pose", "16:9",
     "Female fictional idol striking an exaggerated comedic pose during a "
     "variety game show, giant foam finger prop, bright yellow studio set, "
     "laughing expression mid-shout, vibrant broadcast lighting, energetic "
     "entertainment photography, 35mm lens, " + TAIL),
    ("40", "airport-winter", "3:4",
     "Candid airport photo of a female fictional idol in a long puffer coat, "
     "chunky knit scarf, and beanie pulling a suitcase through a glass "
     "terminal, soft morning light, effortless off-duty style, editorial "
     "street photography, 50mm lens, " + TAIL),
    ("41", "airport-summer", "3:4",
     "Candid airport photo of a female fictional idol in light summer travel "
     "fashion, linen co-ord set with a small crossbody bag and cap, wheeling "
     "a suitcase past terminal windows, natural daylight, off-duty street "
     "style photography, 50mm lens, " + TAIL),
    ("42", "rehearsal-room", "16:9",
     "Girl group of fictional idols rehearsing in a mirror-walled practice "
     "room late at night, casual sweats and sneakers, water bottles and "
     "phones along the wall, one member checking a choreography video, candid "
     "documentary photography, 35mm lens, " + TAIL),
    ("43", "studio-choreography", "16:9",
     "Overhead-angle shot of a girl group of fictional idols running "
     "choreography in a bright dance studio, perfectly synchronized arm "
     "lines reflected in the mirror, soft daylight from tall windows, clean "
     "practice-wear styling, documentary photography, 35mm lens, " + TAIL),
    ("44", "recording-booth", "3:4",
     "Female fictional idol singing into a large studio microphone behind "
     "booth glass, wearing headphones and a cozy hoodie, warm amber studio "
     "lighting, VU meters and pop filter in frame, intimate music-studio "
     "photography, 50mm lens, " + TAIL),
    ("45", "jacket-making-bts", "16:9",
     "Behind-the-scenes making-film shot of a girl group of fictional idols "
     "at an album jacket photoshoot, members laughing between takes on a "
     "pastel studio set, lighting rigs and a photographer silhouette in "
     "frame, candid documentary photography, 35mm lens, " + TAIL),
    ("46", "mv-set-bts", "16:9",
     "Behind-the-scenes shot of a female fictional idol on a music-video set "
     "between takes, sitting on a prop vintage sofa wrapped in a blanket, "
     "crew adjusting lights around her, cinematic set lighting, candid "
     "film-set photography, 35mm lens, " + TAIL),
    ("47", "camping-vlog", "16:9",
     "Vlog-style shot of a girl group of fictional idols at a lakeside "
     "campsite at dusk, string lights strung over a picnic table, members "
     "roasting marshmallows in cozy fleece, tents and a calm lake behind, "
     "warm candid lifestyle photography, 35mm lens, " + TAIL),
    ("48", "fanmeet-finger-heart", "1:1",
     "Female fictional idol at a fan meeting making a double finger-heart "
     "with a bright laugh, pastel stage outfit with a small crown prop, warm "
     "golden lights and soft bokeh crowd behind, heartwarming event "
     "photography, 85mm lens, " + TAIL),
    ("49", "birthday-cafe-display", "1:1",
     "Fan-organized birthday cafe display for a fictional idol, pastel "
     "bunting with star symbols over the counter, a two-tier cream cake with "
     "a gold star topper, a photo wall of candid prints and handwritten "
     "notes, cozy cafe interior, lifestyle photography, " + TAIL),
    ("50", "birthday-cafe-visit", "3:4",
     "Female fictional idol visiting her own fan-organized birthday cafe, "
     "holding a decorated cup with both hands near her face, eyes closed in "
     "delight, pastel star banners and cake display behind her, warm candid "
     "event photography, 50mm lens, " + TAIL),
    ("51", "album-flatlay-pastel", "1:1",
     "Album unboxing flat lay photographed from directly above, generic "
     "pastel mint album sleeve with abstract cloud artwork, glossy photocard "
     "fanned beside a shimmering CD, sticker sheets and washi tape on a linen "
     "desk mat, no readable text or logos, product photography, " + TAIL),
    ("52", "album-flatlay-y2k", "1:1",
     "Y2K-style album flat lay from directly above, generic chrome-finish "
     "album sleeve with a holographic star sticker, clear photocard, mini "
     "poster tube and beaded phone charm on a silver foil background, no "
     "readable text or logos, retro product photography, " + TAIL),
    ("53", "album-unboxing-hands", "1:1",
     "Close-up of hands carefully sliding a photocard out of a generic "
     "pastel album sleeve on a cozy desk, warm lamp light, soft-focus album "
     "components and sleeves scattered around, no readable text or logos, "
     "cozy product photography, " + TAIL),
    ("54", "beauty-prism-rainbow", "1:1",
     "Extreme beauty close-up of a female fictional idol, rainbow prism "
     "light bands sweeping across dewy skin and a glossy eyelid, tiny crystal "
     "freckles on the cheekbone, wet-look glossy lips, high-end beauty "
     "editorial photography, macro 100mm lens, " + TAIL),
    ("55", "beauty-water-light", "1:1",
     "Extreme beauty close-up of a female fictional idol with rippling "
     "water-light reflections dancing across her face as if from a pool, "
     "slicked wet hair, luminous highlighter, serene expression, artistic "
     "beauty editorial photography, macro 100mm lens, " + TAIL),
    ("56", "beauty-glitter-dew", "1:1",
     "Extreme beauty close-up of a female fictional idol with fine gold "
     "glitter dusted along her cheekbone and temple, dewy glass skin, a "
     "single silver hoop catching the light, soft studio glow, luxury beauty "
     "campaign photography, macro 100mm lens, " + TAIL),
    ("57", "photobook-flower-field", "3:4",
     "Photobook spread of a female fictional idol twirling in a wildflower "
     "field at golden hour, flowing pastel sundress, arms out, sun flare "
     "behind her shoulder, dreamy soft-focus hills, editorial fashion "
     "photography, 85mm lens, " + TAIL),
    ("58", "photobook-city-night", "3:4",
     "Photobook concept shot of a female fictional idol on a rainy city "
     "street at night, clear umbrella tilted back, neon signs reflecting in "
     "wet pavement, long trench coat, cinematic nocturnal editorial "
     "photography, 50mm lens, " + TAIL),
    ("59", "photobook-hanbok-modern", "3:4",
     "Modern hanbok concept portrait of a female fictional idol, "
     "contemporary embroidered hanbok in soft jade and ivory with wide "
     "flowing sleeves, standing in a minimalist palace courtyard at dusk, "
     "lanterns glowing softly, high-fashion editorial photography, 85mm lens, "
     + TAIL),
    ("60", "photobook-sporty", "3:4",
     "Sporty photobook concept shot of a female fictional idol in a retro "
     "tracksuit mid-jump on an outdoor basketball court, dynamic freeze-frame, "
     "late-afternoon sun, chain-link fence bokeh, energetic editorial sports "
     "photography, 70mm lens, " + TAIL),
    ("61", "photobook-y2k-retro", "3:4",
     "Y2K retro photobook concept shot of a female fictional idol with twin "
     "buns and butterfly hair clips, shiny silver crop top and low-rise "
     "jeans, leaning on a metallic locker wall, playful retro editorial "
     "photography, 35mm lens, " + TAIL),
    ("62", "photobook-cyber", "3:4",
     "Cyber photobook concept shot of a female fictional idol in a "
     "chrome-accented bodysuit with glowing circuit-line makeup, inside a "
     "dark server-room corridor of blinking lights, futuristic high-fashion "
     "editorial photography, 50mm lens, " + TAIL),
    ("63", "photobook-campus-autumn", "3:4",
     "Campus photobook concept shot of a female fictional idol in a "
     "school-style uniform cardigan sitting on outdoor stone steps with "
     "books, ginkgo leaves falling around her, soft autumn afternoon light, "
     "youthful editorial photography, 85mm lens, " + TAIL),
    ("64", "lightstick-ocean-arena", "16:9",
     "Aerial wide shot of a packed arena at night during a fictional idol "
     "concert, tens of thousands of pastel lightsticks forming a glowing "
     "gradient ocean, tiny stage glowing in the distance, epic cinematic "
     "event photography, 24mm lens, " + TAIL),
    ("65", "festival-fireworks", "16:9",
     "Outdoor festival finale at night, girl group of fictional idols "
     "silhouetted on a glowing main stage as enormous golden fireworks burst "
     "over the crowd, thousands of lightsticks waving below, epic cinematic "
     "concert photography, 24mm lens, " + TAIL),
    ("66", "university-festival", "16:9",
     "Girl group of fictional idols performing at an outdoor university "
     "festival stage at dusk, string lights and scaffolding stage, members "
     "in coordinated casual denim, students cheering with glow sticks, warm "
     "documentary concert photography, 35mm lens, " + TAIL),
    ("67", "christmas-stage", "16:9",
     "Girl group of fictional idols in elegant white-and-red winter gowns "
     "performing on a music-show stage decorated with giant snowflakes and "
     "warm fairy lights, gentle fake snow falling, festive broadcast stage "
     "photography, 35mm lens, " + TAIL),
    ("68", "halloween-stage", "16:9",
     "Girl group of fictional idols in playful Halloween concept costumes "
     "performing on a stage set with giant pumpkins and purple-orange "
     "lighting, cobweb props and fog, energetic broadcast stage photography, "
     "35mm lens, " + TAIL),
    ("69", "water-festival-stage", "16:9",
     "Girl group of fictional idols performing at a summer water festival, "
     "splashing water arcs catching sunlight over the crowd, members "
     "laughing in coordinated sporty stage outfits, energetic outdoor "
     "concert photography, 35mm lens, " + TAIL),
    ("70", "girlgroup-ballad-stage", "16:9",
     "Girl group of fictional idols performing a gentle ballad on a "
     "music-show stage, seated on glowing cube stools in soft flowing "
     "dresses, warm amber lighting and drifting particles, emotional "
     "broadcast stage photography, 50mm lens, " + TAIL),
    ("71", "suit-concept-teaser", "3:4",
     "Suit-concept teaser photo of a female fictional idol in a tailored "
     "black three-piece suit with a loose tie, leaning against a dark studio "
     "wall with dramatic side light, smoldering confident gaze, high-fashion "
     "editorial photography, 85mm lens, " + TAIL),
    ("72", "uniform-concept-teaser", "3:4",
     "School-uniform concept teaser photo of a female fictional idol with a "
     "ribbon tie and pleated skirt, sitting backwards on a classroom chair "
     "with a bright grin, chalk doodles on the board behind, youthful "
     "concept photography, 50mm lens, " + TAIL),
    ("73", "cheerleader-stage", "16:9",
     "Girl group of fictional idols in a cheerleader-themed stage "
     "performance, coordinated blue-and-white uniforms with pom-poms raised "
     "high, jumping in sync on a bright stage, energetic broadcast "
     "performance photography, 35mm lens, " + TAIL),
    ("74", "pajama-sleepover-vlog", "16:9",
     "Vlog-style shot of a girl group of fictional idols in matching pastel "
     "pajamas during a dorm sleepover, members piled on a sofa with snacks "
     "and a movie projected on the wall, fairy lights and pillows everywhere, "
     "cozy candid lifestyle photography, 35mm lens, " + TAIL),
    ("75", "solo-pixie-blonde", "3:4",
     "Studio portrait of a female fictional idol with a platinum blonde "
     "pixie cut, bold red lip and silver eye makeup, black turtleneck, "
     "dramatic split pink-and-blue studio lighting, striking beauty editorial "
     "photography, 85mm lens, " + TAIL),
    ("76", "solo-blue-bob-pastel", "3:4",
     "Studio portrait of a female fictional idol with a pastel blue bob and "
     "blunt bangs, soft ivory knit cardigan, gentle smile looking just past "
     "the camera, clean cream studio backdrop, soft beauty editorial "
     "photography, 85mm lens, " + TAIL),
    ("77", "award-show-tuxedo", "3:4",
     "Female fictional idol in a white tuxedo jacket with a black bow tie on "
     "an award-show red carpet, long earrings catching camera flashes, "
     "poised confident smile, evening glamour editorial photography, 85mm "
     "lens, " + TAIL),
    ("78", "magazine-cover-shoot", "3:4",
     "Magazine cover-style studio shot of a female fictional idol in a "
     "sculptural white blouse, minimalist gold jewelry, sleek pulled-back "
     "hair, clean sage backdrop with no text or logos, high-fashion cover "
     "editorial photography, 85mm lens, " + TAIL),
    ("79", "fansign-instant-photo", "1:1",
     "Female fictional idol at a fan-signing event leaning over the table to "
     "sign an album while smiling up at a fan, cute hair rollers as a "
     "playful prop, warm event lighting, candid fan-event photography, 50mm "
     "lens, " + TAIL),
    ("80", "radio-show-visit", "16:9",
     "Girl group of fictional idols visiting a radio studio, members crammed "
     "around a glass booth table with giant headphones and microphones, "
     "laughing mid-interview, warm broadcast lighting, candid entertainment "
     "photography, 35mm lens, " + TAIL),
    ("81", "encore-bow-thanks", "16:9",
     "Female fictional idol bowing deeply at the end of a solo concert, hand "
     "on heart, ocean of lightsticks glowing in the dark arena behind her, "
     "spotlight halo, emotional concert photography, 85mm lens, " + TAIL),
    ("82", "lightstick-holding-photocard", "3:4",
     "Photocard-style shot of a female fictional idol proudly holding a "
     "glowing pastel star-topped lightstick beside her face, cheeky wink, "
     "fan-meeting stage background in soft bokeh, cute event photography, "
     "85mm lens, " + TAIL),
    ("83", "airport-group-departure", "16:9",
     "Girl group of fictional idols walking together through an airport "
     "terminal in coordinated off-duty travel outfits, rolling suitcases and "
     "cozy hoodies, one member waving at cameras, candid travel street "
     "photography, 35mm lens, " + TAIL),
    ("84", "duo-acoustic-cafe", "16:9",
     "Two-member female fictional idol unit performing an intimate acoustic "
     "session in a tiny cafe, one with a guitar and one singing, string "
     "lights and listeners' silhouettes in the foreground, warm candid live "
     "photography, 35mm lens, " + TAIL),
    ("85", "neon-night-stage", "16:9",
     "Girl group of fictional idols performing under a canopy of neon "
     "signage on a futuristic street-stage set, wet asphalt reflections, "
     "coordinated neon-accent outfits, dynamic mid-choreography freeze, "
     "cinematic stage photography, 35mm lens, " + TAIL),
    ("86", "rainy-window-photocard", "3:4",
     "Moody photocard portrait of a female fictional idol resting her chin "
     "on her hand beside a rain-streaked window, soft grey daylight, cozy "
     "knit sweater, wistful gaze, quiet editorial portrait photography, 85mm "
     "lens, " + TAIL),
    ("87", "photo-booth-fourcut", "1:1",
     "Photo-booth style shot of a four-member fictional idol girl group "
     "squeezed into frame in matching pastel hoodies, playful peace signs "
     "and heart gestures, bright flash lighting, cute candid booth "
     "photography, 35mm lens, " + TAIL),
    ("88", "neon-street-night", "3:4",
     "Night street portrait of a female fictional idol under glowing shop "
     "signage after rain, glossy pavement reflections, silver bomber jacket, "
     "confident stance with hands in pockets, cinematic street editorial "
     "photography, 50mm lens, " + TAIL),
    ("89", "photobook-winter-cabin", "3:4",
     "Winter photobook concept shot of a female fictional idol wrapped in an "
     "oversized knit blanket on a cabin porch, snow-covered pines and string "
     "lights behind, cradling a warm mug, soft morning snow light, cozy "
     "editorial photography, 85mm lens, " + TAIL),
    ("90", "makeup-room-bts", "16:9",
     "Behind-the-scenes shot of a female fictional idol getting final hair "
     "and touch-ups in a dressing room mirror ringed with bulbs, robe and "
     "curlers, stylist hands in frame, warm candid backstage photography, "
     "35mm lens, " + TAIL),
    ("91", "stage-wing-waiting", "16:9",
     "Girl group of fictional idols waiting in the stage wings moments "
     "before going on, peeking at the packed arena through the curtain gap, "
     "hands stacked together in the middle, tense-excited expressions, candid "
     "backstage documentary photography, 35mm lens, " + TAIL),
    ("92", "picnic-vlog-park", "16:9",
     "Vlog-style shot of a girl group of fictional idols at a spring picnic "
     "under cherry blossoms, checkered blanket with fruit and snacks, "
     "members laughing over a board game, petals drifting through sunlight, "
     "candid lifestyle photography, 35mm lens, " + TAIL),
    ("93", "seollal-hanbok-greeting", "3:4",
     "Lunar New Year greeting photo of a female fictional idol in a soft "
     "pink traditional hanbok with golden embroidery, performing a deep "
     "formal bow, warm paper lanterns and pine branches behind, festive "
     "editorial photography, 85mm lens, " + TAIL),
    ("94", "fancam-closeup", "3:4",
     "Fancam-style close-up of a female fictional idol mid-choreography on a "
     "music-show stage, sharp eye contact with the lens, glossy stage "
     "makeup, sequined outfit sparkling under moving lights, dynamic "
     "performance photography, 135mm lens, " + TAIL),
    ("95", "piano-stage-solo", "16:9",
     "Female fictional idol performing a piano ballad at a white grand piano "
     "on a circular stage surrounded by calm water reflections, soft blue "
     "spotlight, audience silhouettes in the foreground, cinematic concert "
     "photography, 85mm lens, " + TAIL),
    ("96", "beauty-morning-light", "1:1",
     "Soft beauty close-up of a female fictional idol with bare dewy skin in "
     "morning window light, tiny hearts drawn in highlighter on her cheek, "
     "fluffy lashes, natural makeup look, fresh clean beauty editorial "
     "photography, macro 100mm lens, " + TAIL),
    ("97", "sports-day-event", "16:9",
     "Girl group of fictional idols at an idol sports-day event, members in "
     "matching tracksuits passing a relay baton on an indoor field, laughing "
     "mid-race, stadium lighting, energetic documentary sports photography, "
     "70mm lens, " + TAIL),
    ("98", "rooftop-golden-hour", "16:9",
     "Girl group of fictional idols on a city rooftop at golden hour, wind "
     "catching coordinated white summer outfits, skyline glowing behind "
     "them, arms over each other's shoulders, cinematic editorial group "
     "photography, 50mm lens, " + TAIL),
    ("99", "finale-group-bow", "16:9",
     "Girl group of fictional idols taking a final bow in a line at the end "
     "of their concert, hands joined and raised together, golden confetti "
     "settling, giant arena screen glow behind, emotional concert "
     "photography, 50mm lens, " + TAIL),
    ("100", "wind-machine-closeup", "3:4",
     "Dramatic close-up of a female fictional idol in front of a wind "
     "machine during a stage performance, hair and ribbon details flying, "
     "intense gaze into the lens, high-contrast stage spotlight, cinematic "
     "performance photography, 135mm lens, " + TAIL),
]


def load_key():
    env = os.path.join(os.path.dirname(HERE), ".env")
    with open(env, encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("NANO_GPT_API_KEY="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    sys.exit("NANO_GPT_API_KEY not found in .env")


def check_balance(key):
    req = urllib.request.Request(
        "https://api.nano-gpt.com/api/check-balance", data=b"{}",
        method="POST",
        headers={"Content-Type": "application/json", "x-api-key": key})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return float(json.load(resp)["usd_balance"])


def jpeg_dims(raw):
    if raw[:2] != b"\xff\xd8":
        return None
    i = 2
    while i + 9 < len(raw):
        if raw[i] != 0xFF:
            i += 1
            continue
        m = raw[i + 1]
        if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:
            i += 2
            continue
        if i + 4 > len(raw):
            return None
        seg = struct.unpack(">H", raw[i + 2:i + 4])[0]
        if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
            h, w = struct.unpack(">HH", raw[i + 5:i + 9])
            return w, h
        i += 2 + seg
    return None


def validate(raw, path):
    dims = jpeg_dims(raw)
    if not dims:
        raise ValueError("not a decodable JPEG")
    if len(raw) < MIN_BYTES:
        raise ValueError(f"file too small ({len(raw)} bytes)")
    return dims


def generate(key, prompt, aspect, resolution):
    body = json.dumps({
        "model": MODEL, "prompt": prompt, "n": 1,
        "resolution": resolution, "aspect_ratio": aspect,
    }).encode()
    last = None
    for attempt in range(2):  # initial try + a single retry on 5xx/429
        req = urllib.request.Request(
            API, data=body, method="POST",
            headers={"Content-Type": "application/json", "x-api-key": key})
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as exc:
            last = exc
            detail = exc.read(200).decode("utf-8", "replace").strip()
            if (exc.code >= 500 or exc.code == 429) and attempt == 0:
                print(f"    HTTP {exc.code}, retrying once in 15s", flush=True)
                time.sleep(15)
                continue
            print(f"    HTTP {exc.code}: {detail}", flush=True)
            return None
        except urllib.error.URLError as exc:
            last = exc
            if attempt == 0:
                print(f"    network error ({exc.reason}), retrying once in 15s",
                      flush=True)
                time.sleep(15)
                continue
            print(f"    network error: {exc}", flush=True)
            return None
    print(f"    giving up: {last}", flush=True)
    return None


def main():
    only = None
    if len(sys.argv) > 2 and sys.argv[1] == "--only":
        only = sys.argv[2]
    key = load_key()
    manifest_path = os.path.join(HERE, "manifest.json")
    entries = []
    if os.path.exists(manifest_path):
        with open(manifest_path, encoding="utf-8") as fh:
            entries = json.load(fh)
    manifest_cats = {e["category"] for e in entries}
    done = {num for num, slug, _a, _p in CATEGORIES if slug in manifest_cats}
    spend = sum(e.get("cost_if_known") or 0 for e in entries)
    print(f"BALANCE BEFORE: ${check_balance(key):.8f} "
          f"(lifetime spend on this set: ${spend:.4f})", flush=True)
    generated = 0
    skipped = 0

    for num, slug, aspect, prompt in CATEGORIES:
        if only and num != only:
            continue
        rel = f"idols/{num}-{slug}.jpg"
        if num in done:
            print(f"== {num}-{slug}: already in manifest, skipping")
            continue
        res = HERO_RESOLUTION if num in HERO_NUMBERS else RESOLUTION
        est = HERO_EST_USD if num in HERO_NUMBERS else PER_IMAGE_EST_USD
        if spend + est > BUDGET_TARGET_USD:
            print(f"STOP: budget target ${BUDGET_TARGET_USD} would be exceeded "
                  f"(spent ${spend:.4f}); halting before {num}-{slug}",
                  flush=True)
            break
        print(f"== {num}-{slug} [{aspect} {res}] generating ...", flush=True)
        data = generate(key, prompt, aspect, res)
        if data is None:
            skipped += 1
            print(f"    SKIPPED {num}-{slug} (request failed)", flush=True)
            continue
        raw = base64.b64decode(data["data"][0]["b64_json"])
        out = os.path.join(HERE, rel)
        try:
            w, h = validate(raw, rel)
        except ValueError as exc:
            print(f"    invalid output ({exc}); regenerating once", flush=True)
            data = generate(key, prompt + ", different composition", aspect, res)
            if data is None:
                skipped += 1
                print(f"    SKIPPED {num}-{slug} (retry request failed)",
                      flush=True)
                continue
            raw = base64.b64decode(data["data"][0]["b64_json"])
            try:
                w, h = validate(raw, rel)
            except ValueError as exc:
                skipped += 1
                print(f"    SKIPPED {num}-{slug}: still invalid ({exc})",
                      flush=True)
                continue
        with open(out, "wb") as fh:
            fh.write(raw)
        cost = data.get("cost")
        spend += cost or 0
        entries.append({
            "file": rel, "category": slug, "aspect_ratio": aspect,
            "width": w, "height": h, "bytes": len(raw), "prompt": prompt,
            "cost_if_known": cost,
        })
        entries.sort(key=lambda e: e["file"])
        with open(manifest_path, "w", encoding="utf-8") as fh:
            json.dump(entries, fh, indent=2)
            fh.write("\n")
        bal = data.get("remainingBalance")
        generated += 1
        print(f"    saved {rel} {w}x{h} {len(raw)}B "
              f"cost=${cost} spend=${spend:.4f} balance=${bal}", flush=True)
        time.sleep(2)  # small gap between sequential requests

    print(f"BALANCE AFTER: ${check_balance(key):.8f}", flush=True)
    print(f"GENERATED: {generated} SKIPPED: {skipped}")
    print(f"TOTAL SPEND (lifetime, this set): ${spend:.4f}")


if __name__ == "__main__":
    main()
