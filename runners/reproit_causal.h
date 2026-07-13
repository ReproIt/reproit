// Shared JSON HTTP causal adapter for instrumented C/C++ applications.
#ifndef REPROIT_CAUSAL_H
#define REPROIT_CAUSAL_H
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int (*ReproItCausalLiveJson)(void* user, const char* method, const char* url,
                                     const char* request_headers_json, const char* request_body_json,
                                     char* response_headers_json, size_t headers_cap,
                                     char* response_body_json, size_t body_cap);

// Call once after routing the application's JSON HTTP client through
// ReproIt_Causal_Json. Outside a Reproit run this is a no-op.
bool ReproIt_Causal_Enable(void);
// Returns HTTP status, or -1 for a capsule miss/invalid response. During replay
// `live` is never called. JSON buffers always contain redacted/replayed JSON.
int ReproIt_Causal_Json(const char* method, const char* url,
                        const char* request_headers_json, const char* request_body_json,
                        char* response_headers_json, size_t headers_cap,
                        char* response_body_json, size_t body_cap,
                        ReproItCausalLiveJson live, void* user);

#ifdef __cplusplus
}
#endif

#ifdef REPROIT_CAUSAL_IMPLEMENTATION
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static bool reproit_causal_on = false;
static unsigned reproit_causal_prior_action = ~0u;
static unsigned reproit_causal_ordinal = 0;
static char* reproit_causal_capsule = NULL;

static bool reproit_causal_secret(const char* key, size_t n) {
    static const char* names[] = {"password","passwd","secret","token","authorization","cookie","email","phone","apikey","publishablekey","privatekey","accesskey","signingkey"};
    char lower[128]; if (n >= sizeof lower) n = sizeof lower - 1;
    size_t w=0; for (size_t i=0;i<n;i++) { char c=(char)tolower((unsigned char)key[i]); if(c!='-'&&c!='_'&&c!='.'&&c!=' ') lower[w++]=c; }
    lower[w]=0;
    for (size_t i=0;i<sizeof names/sizeof names[0];i++) if (strstr(lower,names[i])) return true;
    return false;
}
static const char* reproit_causal_skip_value(const char* p) {
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p=='"') { p++; while (*p) { if (*p=='\\' && p[1]) p+=2; else if (*p++=='"') break; } return p; }
    if (*p=='{' || *p=='[') { char open=*p, close=open=='{'?'}':']'; int depth=0; bool str=false;
        do { char c=*p++; if (str) { if (c=='\\' && *p) p++; else if(c=='"') str=false; }
             else { if(c=='"') str=true; else if(c==open) depth++; else if(c==close) depth--; } } while(*p && depth>0); return p; }
    while (*p && *p!=',' && *p!='}' && *p!=']') p++;
    return p;
}
static void reproit_causal_placeholder(const char* begin,const char* end,char* out,size_t cap){
    while(begin<end&&isspace((unsigned char)*begin))begin++;
    if(begin>=end){snprintf(out,cap,"\"<reproit:null>\"");return;}
    if(*begin=='\"'){
        size_t count=0;const unsigned char*p=(const unsigned char*)begin+1,*stop=(const unsigned char*)end;
        while(p<stop&&*p!='\"'){if(*p=='\\'&&p+1<stop){p+=2;count++;}else{if((*p&0xC0)!=0x80)count++;p++;}}
        snprintf(out,cap,"\"<reproit:string:length=%zu>\"",count);return;
    }
    if(*begin=='{'){snprintf(out,cap,"\"<reproit:object>\"");return;}
    if(*begin=='['){snprintf(out,cap,"\"<reproit:array>\"");return;}
    if(!strncmp(begin,"true",4)||!strncmp(begin,"false",5)){snprintf(out,cap,"\"<reproit:boolean>\"");return;}
    if(!strncmp(begin,"null",4)){snprintf(out,cap,"\"<reproit:null>\"");return;}
    snprintf(out,cap,"\"<reproit:number>\"");
}
// Conservative JSON redactor: secret-key values are replaced as a whole before
// any run artifact is written. Malformed JSON becomes a structural placeholder.
static void reproit_causal_redact(const char* in, char* out, size_t cap) {
    if (!out || !cap) return;
    size_t w=0; const char* p=in?in:"null";
    while (*p && w+1<cap) {
        if (*p!='"') { out[w++]=*p++; continue; }
        const char* start=p++; bool esc=false; while(*p && (esc || *p!='"')) { esc=!esc&&*p=='\\'; if(*p!='\\') esc=false; p++; }
        if (!*p) { snprintf(out,cap,"\"<reproit:invalid-json>\""); return; }
        const char* end=p++; const char* q=p; while(*q&&isspace((unsigned char)*q)) q++;
        bool key=*q==':' && reproit_causal_secret(start+1,(size_t)(end-start-1));
        while(start<p && w+1<cap) out[w++]=*start++;
        if (!key) continue;
        while(p<=q && w+1<cap) out[w++]=*p++;
        while(*p&&isspace((unsigned char)*p)&&w+1<cap) out[w++]=*p++;
        const char*value=p;p=reproit_causal_skip_value(p);char typed[128];reproit_causal_placeholder(value,p,typed,sizeof typed);const char* replacement=typed;
        while(*replacement&&w+1<cap) out[w++]=*replacement++;
    }
    out[w]=0;
}
static void reproit_causal_escape(FILE* f, const char* s) {
    fputc('"',f); for(;s&&*s;s++){ unsigned char c=(unsigned char)*s; if(c=='"'||c=='\\') fputc('\\',f); if(c>=32) fputc(c,f); } fputc('"',f);
}
static unsigned reproit_causal_action(void) {
    const char* path=getenv("REPROIT_ACTION_FILE"); FILE* f=path?fopen(path,"rb"):NULL; unsigned v=0;
    if(f){ fscanf(f,"%u",&v); fclose(f); } return v;
}
static char* reproit_causal_read(const char* path) {
    FILE* f=path?fopen(path,"rb"):NULL; if(!f)return NULL; fseek(f,0,SEEK_END); long n=ftell(f); rewind(f);
    if(n<0||n>16*1024*1024){fclose(f);return NULL;} char* p=(char*)malloc((size_t)n+1); if(!p){fclose(f);return NULL;}
    size_t got=fread(p,1,(size_t)n,f); fclose(f); p[got]=0; return p;
}
static const char* reproit_causal_key(const char* begin,const char* end,const char* key) {
    char pattern[96]; snprintf(pattern,sizeof pattern,"\"%s\"",key); size_t n=strlen(pattern);
    for(const char* p=begin;p&&p+n<end;p++) if(!memcmp(p,pattern,n)){ p+=n; while(p<end&&isspace((unsigned char)*p))p++; if(p<end&&*p==':'){p++;while(p<end&&isspace((unsigned char)*p))p++;return p;} }
    return NULL;
}
static bool reproit_causal_string(const char* b,const char* e,const char* key,const char* wanted) {
    const char* p=reproit_causal_key(b,e,key); if(!p||*p!='"')return false; p++; size_t n=strlen(wanted); return p+n<e&&!memcmp(p,wanted,n)&&p[n]=='"';
}
static int reproit_causal_cmp_part(const void* a,const void* b){return strcmp(*(const char*const*)a,*(const char*const*)b);}
static void reproit_causal_url(const char* raw,char* out,size_t cap){
    if(!cap)return;
    const char*q=strchr(raw?raw:"",'?'); if(!q){snprintf(out,cap,"%s",raw?raw:"");return;}
    size_t base=(size_t)(q-raw); if(base>=cap)base=cap-1;memcpy(out,raw,base);out[base]=0;
    char query[4096];snprintf(query,sizeof query,"%s",q+1);char*parts[128];size_t n=0;char*p=query;
    while(*p&&n<128){parts[n++]=p;char*amp=strchr(p,'&');if(!amp)break;*amp=0;p=amp+1;}
    qsort(parts,n,sizeof parts[0],reproit_causal_cmp_part);size_t w=strlen(out);
    for(size_t i=0;i<n;i++){if(!*parts[i])continue;int wrote=snprintf(out+w,cap-w,"%c%s",w==base?'?':'&',parts[i]);if(wrote<0||(size_t)wrote>=cap-w){out[cap-1]=0;return;}w+=(size_t)wrote;}
}
static unsigned reproit_causal_number(const char* b,const char* e,const char* key) { const char* p=reproit_causal_key(b,e,key); return p?(unsigned)strtoul(p,NULL,10):0; }
static bool reproit_causal_raw(const char* b,const char* e,const char* key,char* out,size_t cap) {
    const char* p=reproit_causal_key(b,e,key); if(!p)return false; const char* q=reproit_causal_skip_value(p); size_t n=(size_t)(q-p); if(n>=cap)n=cap-1; memcpy(out,p,n);out[n]=0;return true;
}
static bool reproit_causal_match(const char* method,const char* url,const char* actor,unsigned action,unsigned ordinal,
                                 char* rh,size_t rhcap,char* rb,size_t rbcap,int* status) {
    if(!reproit_causal_capsule)return false;
    const char* array=strstr(reproit_causal_capsule,"\"exchanges\""); if(!array)return false;
    array=strchr(array,'['); if(!array)return false; bool str=false; int depth=0; const char* start=NULL;
    for(const char* p=array+1;*p;p++){ char c=*p; if(str){if(c=='\\'&&p[1])p++;else if(c=='"')str=false;continue;} if(c=='"'){str=true;continue;}
      if(c=='{'){if(depth++==0)start=p;} else if(c=='}'&&--depth==0&&start){const char* end=p+1;
        char stored_url[4096],expected[4096],actual[4096];const char*stored=reproit_causal_key(start,end,"url");bool url_ok=false;
        if(stored&&*stored=='\"'){stored++;const char*stop=strchr(stored,'\"');if(stop){size_t len=(size_t)(stop-stored);if(len>=sizeof stored_url)len=sizeof stored_url-1;memcpy(stored_url,stored,len);stored_url[len]=0;reproit_causal_url(stored_url,expected,sizeof expected);reproit_causal_url(url,actual,sizeof actual);url_ok=!strcmp(expected,actual);}}
        if(reproit_causal_string(start,end,"actor",actor)&&reproit_causal_string(start,end,"method",method)&&url_ok&&
           (reproit_causal_number(start,end,"actionIndex")==action||reproit_causal_number(start,end,"action_index")==action)&&reproit_causal_number(start,end,"ordinal")==ordinal){
          *status=(int)reproit_causal_number(start,end,"status"); if(!reproit_causal_raw(start,end,"responseHeaders",rh,rhcap))reproit_causal_raw(start,end,"response_headers",rh,rhcap);
          if(!reproit_causal_raw(start,end,"responseBody",rb,rbcap))reproit_causal_raw(start,end,"response_body",rb,rbcap);
          return true; } start=NULL; }
      else if(c==']'&&depth==0)break;
    } return false;
}

bool ReproIt_Causal_Enable(void) {
    const char* network=getenv("REPROIT_NETWORK_FILE"), *capsule=getenv("REPROIT_CAPSULE");
    if(!network&&!capsule)return false;
    reproit_causal_on=true; if(capsule)reproit_causal_capsule=reproit_causal_read(capsule);
    const char* capabilities=getenv("REPROIT_CAPABILITIES_FILE"); if(capabilities){FILE*f=fopen(capabilities,"wb");if(f){fputs("{\"ui_actions\":{\"status\":\"captured\"},\"http\":{\"status\":\"captured\",\"detail\":\"instrumented JSON transport\"},\"http_replay\":{\"status\":\"captured\"}}",f);fclose(f);}}
    return true;
}
int ReproIt_Causal_Json(const char* method,const char* url,const char* qh,const char* qb,char* rh,size_t rhcap,char* rb,size_t rbcap,ReproItCausalLiveJson live,void* user) {
    if(!reproit_causal_on)return live?live(user,method,url,qh,qb,rh,rhcap,rb,rbcap):-1;
    unsigned action=reproit_causal_action(); if(action!=reproit_causal_prior_action){reproit_causal_prior_action=action;reproit_causal_ordinal=0;} unsigned ordinal=reproit_causal_ordinal++;
    const char* actor=getenv("REPROIT_DEVICE");if(!actor)actor="a"; if(reproit_causal_capsule){int status=0;if(reproit_causal_match(method,url,actor,action,ordinal,rh,rhcap,rb,rbcap,&status)){printf("CAPSULE:HIT %s-%u-%u\n",actor,action,ordinal);fflush(stdout);return status;}printf("CAPSULE:MISS %s %s action=%u\n",method,url,action);fflush(stdout);return -1;}
    if(!live)return -1;
    int status=live(user,method,url,qh,qb,rh,rhcap,rb,rbcap); char sqh[4096],sqb[16384],srh[4096],srb[65536];reproit_causal_redact(qh,sqh,sizeof sqh);reproit_causal_redact(qb,sqb,sizeof sqb);reproit_causal_redact(rh,srh,sizeof srh);reproit_causal_redact(rb,srb,sizeof srb);
    const char* path=getenv("REPROIT_NETWORK_FILE");FILE*f=path?fopen(path,"ab"):NULL;if(f){fputs("{\"id\":",f);char id[96],safe_url[4096];snprintf(id,sizeof id,"%s-%u-%u",actor,action,ordinal);reproit_causal_url(url,safe_url,sizeof safe_url);reproit_causal_escape(f,id);fputs(",\"actor\":",f);reproit_causal_escape(f,actor);fprintf(f,",\"actionIndex\":%u,\"ordinal\":%u,\"protocol\":\"http\",\"method\":",action,ordinal);reproit_causal_escape(f,method);fputs(",\"url\":",f);reproit_causal_escape(f,safe_url);fprintf(f,",\"requestHeaders\":%s,\"requestBody\":%s,\"status\":%d,\"responseHeaders\":%s,\"responseBody\":%s,\"required\":true}\n",sqh,sqb,status,srh,srb);fclose(f);}return status;
}
#endif
#endif
