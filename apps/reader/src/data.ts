import { invoke, isTauri } from '@tauri-apps/api/core';
export interface Sentence { id:string; text:string; startUtf8:number; endUtf8:number; startUtf16:number; endUtf16:number }
export interface ReaderDocument { id:string; title:string; language:string; text:string; sentences:Sentence[]; position:number; createdAt:number; sourceHash:string; format:string }
export interface ReviewState { dueAt:number; intervalDays:number; reviews:number }
export interface Word { id:string; word:string; translation:string; language:string; targetLanguage:string; context:string; documentId:string; forward:ReviewState; reverse:ReviewState }
export interface Snapshot {documents:ReaderDocument[]; vocabulary:Word[]}
export interface Original {documentId:string; name:string; bytes:number[]}
export interface Backup {version:number; snapshot:Snapshot; originals:Original[]; reviews?:{entryId:string; direction:string; grade:number; reviewedAt:number}[]}
export interface Voice {id:string; name:string; language:string}
export const LANGUAGES=[['fr','Французька','FR'],['uk','Українська','UA'],['en','Англійська','EN'],['de','Німецька','DE'],['es','Іспанська','ES'],['it','Італійська','IT'],['pt','Португальська','PT'],['pl','Польська','PL'],['nl','Нідерландська','NL'],['cs','Чеська','CZ'],['ro','Румунська','RO'],['sv','Шведська','SE']];
export const SAMPLE_TEXT=`Il y a des matins qui commencent autrement.\n\nCe matin-là, Camille décide de laisser son téléphone à la maison. Elle prend un livre, un carnet et les clés de son vélo. Le ciel est encore rose au-dessus des toits de Paris.\n\nAu coin de la rue, une petite boulangerie ouvre ses portes. L’odeur du pain chaud se mélange à celle du café. Camille s’arrête et commande un croissant.\n\nAvec un sourire, le boulanger lui demande si elle a le temps. Pour la première fois depuis longtemps, elle répond oui.\n\nElle s’installe près de la fenêtre et ouvre son livre. Dehors, la ville se réveille doucement. Et pour une fois, elle n’est pas pressée.`;
export const SAMPLE_TRANSLATIONS=[
'Бувають ранки, які починаються інакше.',
'Того ранку Камій вирішує залишити телефон удома.',
'Вона бере книжку, блокнот і ключі від велосипеда.',
'Небо над дахами Парижа ще рожеве.',
'На розі вулиці маленька пекарня відчиняє двері.',
'Запах теплого хліба змішується із запахом кави.',
'Камій зупиняється й замовляє круасан.',
'Пекар з усмішкою запитує, чи має вона час.',
'Уперше за довгий час вона відповідає «так».',
'Вона сідає біля вікна й відкриває книжку.',
'Надворі місто поволі прокидається.',
'І цього разу вона нікуди не поспішає.'
];
export const LEXICON:Record<string,{translation:string; kind:string; note:string}>={
'matins':{translation:'ранки',kind:'іменник · masculin · pluriel',note:'Однина: le matin — ранок. Des matins — неозначена множина.'},
'autrement':{translation:'інакше',kind:'прислівник',note:'Описує спосіб дії: commencer autrement — починатися інакше.'},
'laisser':{translation:'залишати',kind:'дієслово · infinitif',note:'Laisser quelque chose à la maison — залишити щось удома.'},
'livre':{translation:'книжка',kind:'іменник · masculin',note:'Un livre — книжка. У цьому контексті йдеться про читання.'},
'carnet':{translation:'блокнот',kind:'іменник · masculin',note:'Un carnet — невеликий зошит для записів.'},
'ciel':{translation:'небо',kind:'іменник · masculin',note:'Le ciel est rose — небо рожеве.'},
'boulangerie':{translation:'пекарня',kind:'іменник · féminin',note:'Une boulangerie — магазин, де печуть і продають хліб.'},
'odeur':{translation:'запах',kind:'іменник · féminin',note:'L’odeur de quelque chose — запах чогось.'},
'pain':{translation:'хліб',kind:'іменник · masculin',note:'Du pain chaud — теплого хліба. Du — частковий артикль.'},
'sourire':{translation:'усмішка',kind:'іменник · masculin',note:'Avec un sourire — з усмішкою. Також дієслово «усміхатися».'},
'fenêtre':{translation:'вікно',kind:'іменник · féminin',note:'Près de la fenêtre — біля вікна.'},
'doucement':{translation:'поволі, тихо',kind:'прислівник',note:'У цьому реченні: місто поволі прокидається.'},
'pressée':{translation:'та, що поспішає',kind:'прикметник · féminin',note:'Être pressé / pressée — поспішати. Закінчення -e узгоджується з elle.'}
};
export const emptyReview=():ReviewState=>({dueAt:0,intervalDays:0,reviews:0});
export const desktop=isTauri();
const KEY='lingo-reader-v1';
function segment(text:string,id:string):Sentence[]{const segmenter=new Intl.Segmenter('und',{granularity:'sentence'});return [...segmenter.segment(text)].map(s=>{const value=s.segment.trim();const start=s.index+s.segment.length-s.segment.trimStart().length;return{id:`${id}:${start}`,text:value,startUtf16:start,endUtf16:start+value.length,startUtf8:new TextEncoder().encode(text.slice(0,start)).length,endUtf8:new TextEncoder().encode(text.slice(0,start+value.length)).length};}).filter(s=>s.text);}
function read():Backup{const raw=localStorage.getItem(KEY);return raw?JSON.parse(raw):{version:1,snapshot:{documents:[],vocabulary:[]},originals:[]};}
function write(b:Backup){try{localStorage.setItem(KEY,JSON.stringify(b));}catch{throw new Error('Пам’ять браузера заповнена. Експортуйте резервну копію або відкрийте desktop-застосунок.');}}
export async function api<T>(command:string,args:Record<string,unknown>={}):Promise<T>{
 if(desktop)return invoke<T>(command,args);
 const b=read();let result:unknown;
 switch(command){
 case 'load':return b.snapshot as T;
 case 'import_document':{
  const name=args['name'] as string;const bytes=args['bytes'] as number[];
  if(!name.toLowerCase().endsWith('.txt'))throw new Error('У браузерному перегляді доступний TXT. DOCX і PDF відкривайте в desktop-застосунку.');
  if(bytes.length>2*1024*1024)throw new Error('Ліміт браузерного перегляду — 2 МБ.');
  const text=new TextDecoder('utf-8',{fatal:true}).decode(new Uint8Array(bytes));if(!text.trim())throw new Error('Документ порожній.');
  const hash=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new Uint8Array(bytes)))).map(n=>n.toString(16).padStart(2,'0')).join('');
  const old=b.snapshot.documents.find(d=>d.sourceHash===hash);if(old)return old as T;
  const id=crypto.randomUUID();const doc:ReaderDocument={id,title:name.replace(/\.[^.]+$/,''),text,language:args['language'] as string,sentences:segment(text,id),position:0,createdAt:Date.now()/1000,sourceHash:hash,format:'txt'};
  b.snapshot.documents.unshift(doc);b.originals.push({documentId:id,name,bytes});result=doc;break;
 }
 case 'save_position':{const doc=b.snapshot.documents.find(d=>d.id===args['id']);if(doc)doc.position=args['position'] as number;break;}
 case 'save_word':{const word=args['entry'] as Word;const existing=b.snapshot.vocabulary.find(v=>v.word.toLocaleLowerCase()===word.word.toLocaleLowerCase()&&v.translation.toLocaleLowerCase()===word.translation.toLocaleLowerCase()&&v.language===word.language&&v.targetLanguage===word.targetLanguage);if(existing)return existing as T;word.id=crypto.randomUUID();word.forward=emptyReview();word.reverse=emptyReview();b.snapshot.vocabulary.unshift(word);result=word;break;}
 case 'review':{const v=b.snapshot.vocabulary.find(v=>v.id===args['id']);if(!v)throw new Error('Картку не знайдено');const s=args['direction']==='forward'?v.forward:v.reverse;const grade=args['grade'] as number;s.intervalDays=grade===0?0:grade===1?1:grade===2?(s.intervalDays?Math.min(365,s.intervalDays*2):2):(s.intervalDays?Math.min(365,s.intervalDays*3):4);s.dueAt=Date.now()/1000+(grade===0?60:s.intervalDays*86400);s.reviews++;(b.reviews??=[]).push({entryId:v.id,direction:args['direction'] as string,grade,reviewedAt:Math.floor(Date.now()/1000)});break;}
 case 'export_backup':return b as T;
 case 'original':return b.originals.find(o=>o.documentId===args['id']) as T;
 case 'restore_backup':{const incoming=args['backup'] as Backup;
  if(incoming.version!==1||!Array.isArray(incoming.snapshot?.documents)||!Array.isArray(incoming.snapshot?.vocabulary)||!Array.isArray(incoming.originals))throw new Error('Некоректний архів');
  const validated:ReaderDocument[]=[];
  for(const d of incoming.snapshot.documents){const o=incoming.originals.find(o=>o.documentId===d.id);if(!o||o.bytes.length>2*1024*1024||!LANGUAGES.some(l=>l[0]===d.language))throw new Error('Некоректний документ в архіві');const hash=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new Uint8Array(o.bytes)))).map(n=>n.toString(16).padStart(2,'0')).join('');if(hash!==d.sourceHash)throw new Error('Пошкоджений оригінал');if(d.format!=='txt')throw new Error('Для відновлення PDF/DOCX відкрийте desktop-застосунок');const text=new TextDecoder('utf-8',{fatal:true}).decode(new Uint8Array(o.bytes));if(!text.trim())throw new Error('Порожній документ');validated.push({...d,text,sentences:segment(text,d.id),position:Math.max(0,Math.min(d.position,segment(text,d.id).length-1))});}
  for(const v of incoming.snapshot.vocabulary){if(!v.word?.trim()||!v.translation?.trim()||!v.forward||!v.reverse||!LANGUAGES.some(l=>l[0]===v.language)||!LANGUAGES.some(l=>l[0]===v.targetLanguage))throw new Error('Некоректна картка');}
  for(const d of validated){if(!b.snapshot.documents.some(x=>x.id===d.id||x.sourceHash===d.sourceHash)){b.snapshot.documents.push(d);b.originals.push(incoming.originals.find(o=>o.documentId===d.id)!);}}
  for(const r of incoming.reviews||[]){if(!['forward','reverse'].includes(r.direction)||!Number.isInteger(r.grade)||r.grade<0||r.grade>3)throw new Error('Некоректна історія повторень');if(!(b.reviews??=[]).some(x=>x.entryId===r.entryId&&x.direction===r.direction&&x.reviewedAt===r.reviewedAt&&x.grade===r.grade))b.reviews.push(r);}
  for(const v of incoming.snapshot.vocabulary){if(!b.snapshot.vocabulary.some(x=>x.id===v.id||(x.word===v.word&&x.translation===v.translation&&x.language===v.language&&x.targetLanguage===v.targetLanguage)))b.snapshot.vocabulary.push(v);}break;}
 default:throw new Error(`Невідома операція: ${command}`);
 }
 write(b);return result as T;
}
