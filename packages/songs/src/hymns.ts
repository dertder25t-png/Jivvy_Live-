/**
 * The public-domain hymn library, free on every license. Every text here was published before
 * 1929, so it is public domain in the United States; translations are the old public-domain
 * ones. Written with section headers, so each goes through the same parser as pasted lyrics.
 * Loaded only when the operator opens the library.
 */
import { songFromLyrics, type Song } from "./index";

export interface Hymn {
  id: string;
  title: string;
  author: string;
  /** When the words were first published. */
  year: number;
  lyrics: string;
}

const PD = "Public domain";

export const HYMNS: Hymn[] = [
  {
    id: "doxology",
    title: "Doxology",
    author: "Thomas Ken",
    year: 1674,
    lyrics: `Verse 1
Praise God, from whom all blessings flow;
Praise Him, all creatures here below;
Praise Him above, ye heavenly host;
Praise Father, Son, and Holy Ghost.

Ending
Amen.`,
  },
  {
    id: "amazing-grace",
    title: "Amazing Grace",
    author: "John Newton",
    year: 1779,
    lyrics: `Verse 1
Amazing grace! How sweet the sound
That saved a wretch like me!
I once was lost, but now am found;
Was blind, but now I see.

Verse 2
'Twas grace that taught my heart to fear,
And grace my fears relieved;
How precious did that grace appear
The hour I first believed!

Verse 3
Through many dangers, toils, and snares,
I have already come;
'Tis grace hath brought me safe thus far,
And grace will lead me home.

Verse 4
The Lord has promised good to me,
His word my hope secures;
He will my shield and portion be,
As long as life endures.`,
  },
  {
    id: "blessed-assurance",
    title: "Blessed Assurance",
    author: "Fanny Crosby",
    year: 1873,
    lyrics: `Verse 1
Blessed assurance, Jesus is mine!
O what a foretaste of glory divine!
Heir of salvation, purchase of God,
Born of His Spirit, washed in His blood.

Refrain
This is my story, this is my song,
Praising my Savior all the day long;
This is my story, this is my song,
Praising my Savior all the day long.

Verse 2
Perfect submission, perfect delight,
Visions of rapture now burst on my sight;
Angels descending bring from above
Echoes of mercy, whispers of love.

Verse 3
Perfect submission, all is at rest;
I in my Savior am happy and blest,
Watching and waiting, looking above,
Filled with His goodness, lost in His love.`,
  },
  {
    id: "rock-of-ages",
    title: "Rock of Ages",
    author: "Augustus Toplady",
    year: 1776,
    lyrics: `Verse 1
Rock of Ages, cleft for me,
Let me hide myself in Thee;
Let the water and the blood,
From Thy wounded side which flowed,
Be of sin the double cure,
Save from wrath and make me pure.

Verse 2
Not the labors of my hands
Can fulfill Thy law's demands;
Could my zeal no respite know,
Could my tears forever flow,
All for sin could not atone;
Thou must save, and Thou alone.

Verse 3
Nothing in my hand I bring,
Simply to the cross I cling;
Naked, come to Thee for dress;
Helpless, look to Thee for grace;
Foul, I to the fountain fly;
Wash me, Savior, or I die.`,
  },
  {
    id: "it-is-well",
    title: "It Is Well with My Soul",
    author: "Horatio Spafford",
    year: 1873,
    lyrics: `Verse 1
When peace like a river attendeth my way,
When sorrows like sea billows roll;
Whatever my lot, Thou hast taught me to say,
It is well, it is well with my soul.

Refrain
It is well with my soul,
It is well, it is well with my soul.

Verse 2
Though Satan should buffet, though trials should come,
Let this blest assurance control,
That Christ has regarded my helpless estate,
And hath shed His own blood for my soul.

Verse 3
And Lord, haste the day when my faith shall be sight,
The clouds be rolled back as a scroll;
The trump shall resound, and the Lord shall descend,
Even so, it is well with my soul.`,
  },
  {
    id: "come-thou-fount",
    title: "Come, Thou Fount of Every Blessing",
    author: "Robert Robinson",
    year: 1758,
    lyrics: `Verse 1
Come, Thou Fount of every blessing,
Tune my heart to sing Thy grace;
Streams of mercy, never ceasing,
Call for songs of loudest praise.
Teach me some melodious sonnet,
Sung by flaming tongues above.
Praise the mount! I'm fixed upon it,
Mount of Thy redeeming love.

Verse 2
O to grace how great a debtor
Daily I'm constrained to be!
Let Thy goodness, like a fetter,
Bind my wandering heart to Thee.
Prone to wander, Lord, I feel it,
Prone to leave the God I love;
Here's my heart, O take and seal it,
Seal it for Thy courts above.`,
  },
  {
    id: "joyful-joyful",
    title: "Joyful, Joyful, We Adore Thee",
    author: "Henry van Dyke",
    year: 1907,
    lyrics: `Verse 1
Joyful, joyful, we adore Thee,
God of glory, Lord of love;
Hearts unfold like flowers before Thee,
Opening to the sun above.
Melt the clouds of sin and sadness;
Drive the dark of doubt away;
Giver of immortal gladness,
Fill us with the light of day!

Verse 2
All Thy works with joy surround Thee,
Earth and heaven reflect Thy rays,
Stars and angels sing around Thee,
Center of unbroken praise.
Field and forest, vale and mountain,
Flowery meadow, flashing sea,
Singing bird and flowing fountain
Call us to rejoice in Thee.`,
  },
];

/** A hymn as a song, ready for the run sheet. */
export function hymnSong(hymn: Hymn): Song {
  return songFromLyrics(hymn.title, hymn.lyrics, { author: `${hymn.author} (${hymn.year})`, copyright: PD });
}
