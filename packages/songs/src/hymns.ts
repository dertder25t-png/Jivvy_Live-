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
  {
    id: "holy-holy-holy",
    title: "Holy, Holy, Holy",
    author: "Reginald Heber",
    year: 1826,
    lyrics: `Verse 1
Holy, holy, holy! Lord God Almighty!
Early in the morning our song shall rise to Thee;
Holy, holy, holy, merciful and mighty,
God in three Persons, blessed Trinity!

Verse 2
Holy, holy, holy! All the saints adore Thee,
Casting down their golden crowns around the glassy sea;
Cherubim and seraphim falling down before Thee,
Which wert and art and evermore shalt be.

Verse 3
Holy, holy, holy! Though the darkness hide Thee,
Though the eye of sinful man Thy glory may not see,
Only Thou art holy; there is none beside Thee,
Perfect in power, in love, and purity.

Verse 4
Holy, holy, holy! Lord God Almighty!
All Thy works shall praise Thy name in earth, and sky, and sea;
Holy, holy, holy, merciful and mighty,
God in three Persons, blessed Trinity!`,
  },
  {
    id: "be-thou-my-vision",
    title: "Be Thou My Vision",
    author: "Mary Byrne (trans. Eleanor Hull)",
    year: 1912,
    lyrics: `Verse 1
Be Thou my Vision, O Lord of my heart;
Naught be all else to me, save that Thou art;
Thou my best Thought, by day or by night,
Waking or sleeping, Thy presence my light.

Verse 2
Be Thou my Wisdom, and Thou my true Word;
I ever with Thee and Thou with me, Lord;
Thou my great Father, I Thy true son;
Thou in me dwelling, and I with Thee one.

Verse 3
High King of heaven, my victory won,
May I reach heaven's joys, O bright Heaven's Sun!
Heart of my own heart, whatever befall,
Still be my Vision, O Ruler of all.`,
  },
  {
    id: "crown-him",
    title: "Crown Him with Many Crowns",
    author: "Matthew Bridges, Godfrey Thring",
    year: 1851,
    lyrics: `Verse 1
Crown Him with many crowns,
The Lamb upon His throne;
Hark! how the heavenly anthem drowns
All music but its own!
Awake, my soul, and sing
Of Him who died for thee,
And hail Him as thy matchless King
Through all eternity.

Verse 2
Crown Him the Lord of love;
Behold His hands and side,
Those wounds, yet visible above,
In beauty glorified:
All hail, Redeemer, hail!
For Thou hast died for me;
Thy praise and glory shall not fail
Through all eternity.`,
  },
  {
    id: "fairest-lord-jesus",
    title: "Fairest Lord Jesus",
    author: "Joseph A. Seiss (trans.)",
    year: 1873,
    lyrics: `Verse 1
Fairest Lord Jesus,
Ruler of all nature,
O Thou of God and man the Son,
Thee will I cherish,
Thee will I honor,
Thou, my soul's glory, joy, and crown.

Verse 2
Fair are the meadows,
Fairer still the woodlands,
Robed in the blooming garb of spring:
Jesus is fairer,
Jesus is purer,
Who makes the woeful heart to sing.

Verse 3
Fair is the sunshine,
Fairer still the moonlight,
And all the twinkling starry host:
Jesus shines brighter,
Jesus shines purer
Than all the angels heaven can boast.`,
  },
  {
    id: "o-worship-the-king",
    title: "O Worship the King",
    author: "Robert Grant",
    year: 1833,
    lyrics: `Verse 1
O worship the King, all glorious above,
O gratefully sing His power and His love;
Our Shield and Defender, the Ancient of Days,
Pavilioned in splendor, and girded with praise.

Verse 2
O tell of His might, O sing of His grace,
Whose robe is the light, whose canopy space;
His chariots of wrath the deep thunderclouds form,
And dark is His path on the wings of the storm.

Verse 3
Thy bountiful care, what tongue can recite?
It breathes in the air, it shines in the light;
It streams from the hills, it descends to the plain,
And sweetly distills in the dew and the rain.

Verse 4
Frail children of dust, and feeble as frail,
In Thee do we trust, nor find Thee to fail;
Thy mercies how tender, how firm to the end,
Our Maker, Defender, Redeemer, and Friend.`,
  },
  {
    id: "praise-to-the-lord",
    title: "Praise to the Lord, the Almighty",
    author: "Joachim Neander (trans. Catherine Winkworth)",
    year: 1863,
    lyrics: `Verse 1
Praise to the Lord, the Almighty, the King of creation!
O my soul, praise Him, for He is thy health and salvation!
All ye who hear, now to His temple draw near,
Joining in glad adoration.

Verse 2
Praise to the Lord, who o'er all things so wondrously reigneth,
Shelters thee under His wings, yea, so gently sustaineth;
Hast thou not seen how thy desires e'er have been
Granted in what He ordaineth?`,
  },
  {
    id: "all-hail-the-power",
    title: "All Hail the Power of Jesus' Name",
    author: "Edward Perronet",
    year: 1779,
    lyrics: `Verse 1
All hail the power of Jesus' name!
Let angels prostrate fall;
Bring forth the royal diadem,
And crown Him Lord of all.

Verse 2
Crown Him, ye martyrs of your God,
Who from His altar call;
Extol the stem of Jesse's rod,
And crown Him Lord of all.

Verse 3
Ye seed of Israel's chosen race,
Ye ransomed of the fall,
Hail Him who saves you by His grace,
And crown Him Lord of all.

Verse 4
Let every kindred, every tribe,
On this terrestrial ball,
To Him all majesty ascribe,
And crown Him Lord of all.`,
  },
  {
    id: "what-a-friend",
    title: "What a Friend We Have in Jesus",
    author: "Joseph Scriven",
    year: 1855,
    lyrics: `Verse 1
What a friend we have in Jesus,
All our sins and griefs to bear!
What a privilege to carry
Everything to God in prayer!
O what peace we often forfeit,
O what needless pain we bear,
All because we do not carry
Everything to God in prayer!

Verse 2
Have we trials and temptations?
Is there trouble anywhere?
We should never be discouraged;
Take it to the Lord in prayer.
Can we find a friend so faithful
Who will all our sorrows share?
Jesus knows our every weakness;
Take it to the Lord in prayer.

Verse 3
Are we weak and heavy laden,
Cumbered with a load of care?
Precious Savior, still our refuge,
Take it to the Lord in prayer.
Do thy friends despise, forsake thee?
Take it to the Lord in prayer!
In His arms He'll take and shield thee;
Thou wilt find a solace there.`,
  },
  {
    id: "abide-with-me",
    title: "Abide with Me",
    author: "Henry Francis Lyte",
    year: 1847,
    lyrics: `Verse 1
Abide with me; fast falls the eventide;
The darkness deepens; Lord, with me abide.
When other helpers fail and comforts flee,
Help of the helpless, O abide with me.

Verse 2
Swift to its close ebbs out life's little day;
Earth's joys grow dim, its glories pass away;
Change and decay in all around I see;
O Thou who changest not, abide with me.

Verse 3
I need Thy presence every passing hour;
What but Thy grace can foil the tempter's power?
Who like Thyself my guide and stay can be?
Through cloud and sunshine, O abide with me.`,
  },
  {
    id: "love-divine",
    title: "Love Divine, All Loves Excelling",
    author: "Charles Wesley",
    year: 1747,
    lyrics: `Verse 1
Love divine, all loves excelling,
Joy of heaven, to earth come down;
Fix in us Thy humble dwelling,
All Thy faithful mercies crown.
Jesus, Thou art all compassion,
Pure, unbounded love Thou art;
Visit us with Thy salvation;
Enter every trembling heart.

Verse 2
Breathe, O breathe Thy loving Spirit
Into every troubled breast;
Let us all in Thee inherit,
Let us find that second rest.

Verse 3
Finish, then, Thy new creation;
Pure and spotless let us be;
Let us see Thy great salvation
Perfectly restored in Thee;
Changed from glory into glory,
Till in heaven we take our place,
Till we cast our crowns before Thee,
Lost in wonder, love, and praise.`,
  },
  {
    id: "i-need-thee",
    title: "I Need Thee Every Hour",
    author: "Annie S. Hawks",
    year: 1872,
    lyrics: `Verse 1
I need Thee every hour,
Most gracious Lord;
No tender voice like Thine
Can peace afford.

Refrain
I need Thee, O I need Thee;
Every hour I need Thee;
O bless me now, my Savior,
I come to Thee.

Verse 2
I need Thee every hour,
Stay Thou near by;
Temptations lose their power
When Thou art nigh.

Verse 3
I need Thee every hour,
In joy or pain;
Come quickly and abide,
Or life is in vain.`,
  },
  {
    id: "be-still-my-soul",
    title: "Be Still, My Soul",
    author: "Katharina von Schlegel (trans. Jane Borthwick)",
    year: 1855,
    lyrics: `Verse 1
Be still, my soul: the Lord is on thy side;
Bear patiently the cross of grief or pain;
Leave to thy God to order and provide;
In every change He faithful will remain.
Be still, my soul: thy best, thy heavenly Friend
Through thorny ways leads to a joyful end.

Verse 2
Be still, my soul: thy God doth undertake
To guide the future as He has the past;
Thy hope, thy confidence let nothing shake;
All now mysterious shall be bright at last.`,
  },
  {
    id: "guide-me",
    title: "Guide Me, O Thou Great Jehovah",
    author: "William Williams (trans. Peter Williams)",
    year: 1771,
    lyrics: `Verse 1
Guide me, O Thou great Jehovah,
Pilgrim through this barren land;
I am weak, but Thou art mighty;
Hold me with Thy powerful hand.

Refrain
Bread of heaven,
Feed me till I want no more.

Verse 2
Open now the crystal fountain,
Whence the healing stream doth flow;
Let the fiery, cloudy pillar
Lead me all my journey through.`,
  },
  {
    id: "for-the-beauty-of-the-earth",
    title: "For the Beauty of the Earth",
    author: "Folliott S. Pierpoint",
    year: 1864,
    lyrics: `Verse 1
For the beauty of the earth,
For the glory of the skies,
For the love which from our birth
Over and around us lies,
Lord of all, to Thee we raise
This our hymn of grateful praise.

Verse 2
For the joy of human love,
Brother, sister, parent, child,
Friends on earth, and friends above,
For all gentle thoughts and mild,
Lord of all, to Thee we raise
This our hymn of grateful praise.`,
  },
  {
    id: "now-the-day-is-over",
    title: "Now the Day Is Over",
    author: "Sabine Baring-Gould",
    year: 1865,
    lyrics: `Verse 1
Now the day is over,
Night is drawing nigh;
Shadows of the evening
Steal across the sky.

Verse 2
Jesus, give the weary
Calm and sweet repose;
With Thy tenderest blessing
May our eyelids close.`,
  },
];

/** A hymn as a song, ready for the run sheet. */
export function hymnSong(hymn: Hymn): Song {
  return songFromLyrics(hymn.title, hymn.lyrics, { author: `${hymn.author} (${hymn.year})`, copyright: PD });
}
