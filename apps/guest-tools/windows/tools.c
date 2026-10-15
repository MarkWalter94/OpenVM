/* LightVM Tools guest component (XP SP3 and later). MIT.
 * Direct Win32 entry point, no C/Rust runtime DLLs, no remote shell.
 * All wire buffers and UI trees have fixed upper bounds.
 */
#define WIN32_LEAN_AND_MEAN
#define COBJMACROS
#define CINTERFACE
#define _WIN32_WINNT 0x0501
#include <winsock2.h>
#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <winnetwk.h>
#include <wincrypt.h>
#include <uiautomation.h>
#include <stdint.h>
#include "json.h"

/* SHA-256 is available from the RSA/AES provider on XP SP3 even though
   newer SDK headers gate this identifier on the Vista target macro. */
#ifndef CALG_SHA_256
#define CALG_SHA_256 0x0000800c
#endif

#define FRAME (256u * 1024u)
#define CHUNK (64u * 1024u)
#define TOKEN_COUNT 8192
#define NODE_COUNT 1000
#define PATH_CAP 1024

/* Freestanding support; -fno-builtin prevents recursive compiler lowering. */
void *memcpy(void *dst, const void *src, size_t n) { unsigned char *d=dst; const unsigned char *s=src; size_t i; for(i=0;i<n;++i)d[i]=s[i]; return dst; }
void *memset(void *dst, int value, size_t n) { unsigned char *d=dst; size_t i; for(i=0;i<n;++i)d[i]=(unsigned char)value; return dst; }
void *memmove(void *dst, const void *src, size_t n) { unsigned char *d=dst; const unsigned char *s=src; size_t i; if(d<s)for(i=0;i<n;++i)d[i]=s[i]; else for(i=n;i;--i)d[i-1]=s[i-1]; return dst; }
size_t strlen(const char *s) { size_t n=0; while(s[n])++n; return n; }
static int equal(const char *a,const char *b) { while(*a&&*a==*b){++a;++b;}return *a==*b; }
static int starts(const char *a,const char *b) { while(*b)if(*a++!=*b++)return 0; return 1; }
static int token_kind(const JDoc *d,int token){return token>=0&&(size_t)token<d->count?d->tokens[token].kind:-1;}
static void *alloc(size_t n) { return HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, n); }
static void release(void *p,size_t n) { if(p){SecureZeroMemory(p,n);HeapFree(GetProcessHeap(),0,p);} }
static int wide(const char *s,WCHAR *out,int capacity) { return MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s,-1,out,capacity)>0; }
static size_t wlength(const WCHAR *s){size_t n=0;while(s[n])++n;return n;}
static int wappend(WCHAR *dst,size_t capacity,const WCHAR *src){size_t n=wlength(dst),m=wlength(src);if(n+m>=capacity)return 0;memcpy(dst+n,src,(m+1)*sizeof(WCHAR));return 1;}
static void message(const WCHAR *text){MessageBoxW(NULL,text,L"LightVM Tools",MB_OK|MB_ICONINFORMATION);}

typedef struct {
    char vm[80],run[80],credential[65],control_credential[65];
    unsigned files_port,control_port;
    char shares[32][49]; unsigned share_count;
} Bootstrap;
static Bootstrap bootstrap;
static volatile LONG running=1;
static HCRYPTPROV crypto;
static WCHAR guest_root[PATH_CAP];
static CRITICAL_SECTION file_lock;

typedef struct { SOCKET socket; char *input,*output; JToken *tokens; JDoc doc; } Channel;
static int io_all(SOCKET socket,char *bytes,size_t size,int writing){
    while(size){int n=writing?send(socket,bytes,(int)size,0):recv(socket,bytes,(int)size,0);if(n<=0)return 0;bytes+=n;size-=(size_t)n;}return 1;
}
static int frame_send(Channel *c,JOut *out){
    uint32_t size;
    if(!out->ok||!out->length||out->length>FRAME)return 0;
    size=htonl((uint32_t)out->length);
    return io_all(c->socket,(char *)&size,4,1)&&io_all(c->socket,out->data,out->length,1);
}
static int frame_receive(Channel *c){
    uint32_t size;
    if(!io_all(c->socket,(char *)&size,4,0))return 0;
    size=ntohl(size);if(!size||size>FRAME)return 0;
    if(!io_all(c->socket,c->input,size,0))return 0;
    return jparse(&c->doc,c->input,size,c->tokens,TOKEN_COUNT);
}
static int channel_init(Channel *c){
    memset(c,0,sizeof(*c));c->socket=INVALID_SOCKET;
    c->input=alloc(FRAME);c->output=alloc(FRAME);c->tokens=alloc(sizeof(JToken)*TOKEN_COUNT);
    return c->input&&c->output&&c->tokens;
}
static void channel_close(Channel *c){if(c->socket!=INVALID_SOCKET){shutdown(c->socket,SD_BOTH);closesocket(c->socket);c->socket=INVALID_SOCKET;}}
static int channel_connect(Channel *c,int control){
    struct sockaddr_in address; JOut out; char hello[1024];int yes=1; DWORD timeout=5000;
    channel_close(c); c->socket=socket(AF_INET,SOCK_STREAM,IPPROTO_TCP);
    if(c->socket==INVALID_SOCKET)return 0;
    setsockopt(c->socket,IPPROTO_TCP,TCP_NODELAY,(const char *)&yes,sizeof(yes));
    setsockopt(c->socket,SOL_SOCKET,SO_RCVTIMEO,(const char *)&timeout,sizeof(timeout));
    setsockopt(c->socket,SOL_SOCKET,SO_SNDTIMEO,(const char *)&timeout,sizeof(timeout));
    memset(&address,0,sizeof(address)); address.sin_family=AF_INET;
    address.sin_addr.s_addr=inet_addr("10.0.2.99");
    address.sin_port=htons((u_short)(control?bootstrap.control_port:bootstrap.files_port));
    /* Nonblocking connect has a deadline even on an unreachable guest NIC. */
    { u_long nonblocking=1; fd_set write_set,error_set; struct timeval wait={5,0}; int err=0,len=sizeof(err);
      ioctlsocket(c->socket,FIONBIO,&nonblocking);
      if(connect(c->socket,(struct sockaddr *)&address,sizeof(address))!=0&&WSAGetLastError()!=WSAEWOULDBLOCK){channel_close(c);return 0;}
      FD_ZERO(&write_set);FD_ZERO(&error_set);FD_SET(c->socket,&write_set);FD_SET(c->socket,&error_set);
      if(select(0,NULL,&write_set,&error_set,&wait)<=0||FD_ISSET(c->socket,&error_set)||getsockopt(c->socket,SOL_SOCKET,SO_ERROR,(char *)&err,&len)||err){channel_close(c);return 0;}
      nonblocking=0;ioctlsocket(c->socket,FIONBIO,&nonblocking);
    }
    jout(&out,hello,sizeof(hello));jtext(&out,"{\"version\":1,\"vm\":");jquote(&out,bootstrap.vm);
    jtext(&out,",\"run\":");jquote(&out,bootstrap.run);jtext(&out,",\"credential\":");
    jquote(&out,control?bootstrap.control_credential:bootstrap.credential);jtext(&out,"}");
    {int sent=frame_send(c,&out);SecureZeroMemory(hello,sizeof(hello));if(!sent||!frame_receive(c)||jnumber(&c->doc,jget(&c->doc,0,"version"),0)!=1){channel_close(c);return 0;}}
    if(control){timeout=0;setsockopt(c->socket,SOL_SOCKET,SO_RCVTIMEO,(const char *)&timeout,sizeof(timeout));}
    return 1;
}
static void error_reply(JOut *out,const char *code){jtext(out,"{\"result\":\"error\",\"code\":");jquote(out,code);jtext(out,"}");}
static int file_call(Channel *c,JOut *request){
    if(c->socket==INVALID_SOCKET&&!channel_connect(c,0))return 0;
    if(!frame_send(c,request)||!frame_receive(c)){channel_close(c);return 0;}return 1;
}
static int result_is(Channel *c,const char *value){return jequal(&c->doc,jget(&c->doc,0,"result"),value);}

static int read_bootstrap(void){
    unsigned drive; char *data=alloc(16385); JToken *tokens=alloc(sizeof(JToken)*1024); JDoc doc; int ok=0;
    if(!data||!tokens)goto done;
    for(drive=2;drive<26&&!ok;++drive){
        WCHAR root[4]={(WCHAR)('A'+drive),L':',L'\\',0},path[64],label[64]; HANDLE file; DWORD count;
        if(GetDriveTypeW(root)!=DRIVE_CDROM)continue;
        if(!GetVolumeInformationW(root,label,64,NULL,NULL,NULL,NULL,0)||lstrcmpiW(label,L"LIGHTVM_TOOLS")!=0)continue;
        lstrcpyW(path,root);lstrcatW(path,L"TOOLS.CFG");
        file=CreateFileW(path,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
        if(file==INVALID_HANDLE_VALUE)continue;
        if(!ReadFile(file,data,16385,&count,NULL)||count>16384){CloseHandle(file);continue;}
        CloseHandle(file);
        if(!jparse(&doc,data,count,tokens,1024)||jnumber(&doc,jget(&doc,0,"version"),0)!=1||!jequal(&doc,jget(&doc,0,"host"),"10.0.2.99"))continue;
        if(!jstring(&doc,jget(&doc,0,"vm"),bootstrap.vm,sizeof(bootstrap.vm))||!jstring(&doc,jget(&doc,0,"run"),bootstrap.run,sizeof(bootstrap.run))
            ||!jstring(&doc,jget(&doc,0,"credential"),bootstrap.credential,65)||!jstring(&doc,jget(&doc,0,"control_credential"),bootstrap.control_credential,65))continue;
        if(strlen(bootstrap.credential)!=64||strlen(bootstrap.control_credential)!=64)continue;
        bootstrap.files_port=(unsigned)jnumber(&doc,jget(&doc,0,"files_port"),0);
        bootstrap.control_port=(unsigned)jnumber(&doc,jget(&doc,0,"control_port"),0);
        if(!bootstrap.files_port||bootstrap.files_port>65535||!bootstrap.control_port||bootstrap.control_port>65535)continue;
        {int array=jget(&doc,0,"shares");size_t i;bootstrap.share_count=0;
         for(i=(size_t)(array+1);array>=0&&i<doc.count;++i){
            if(doc.tokens[i].start>=doc.tokens[array].end)break;
            if(doc.tokens[i].parent==array&&doc.tokens[i].kind==J_OBJECT&&bootstrap.share_count<32){
                char *name=bootstrap.shares[bootstrap.share_count];size_t n;
                if(!jstring(&doc,jget(&doc,(int)i,"name"),name,49))break;
                for(n=0;name[n];++n)if(!((name[n]>='a'&&name[n]<='z')||(name[n]>='A'&&name[n]<='Z')||(name[n]>='0'&&name[n]<='9')||name[n]=='_'||name[n]=='-'))break;
                if(!name[n]&&n)++bootstrap.share_count;
            }
         }
        }
        ok=1;
    }
done:release(data,16385);release(tokens,sizeof(JToken)*1024);return ok;
}

/* UI Automation objects live only on the MTA control thread. Each tree is
 * bound to the monotonically increasing request id supplied by the host. */
static IUIAutomation *automation;
static IUIAutomationTreeWalker *walker;
static IUIAutomationElement *elements[NODE_COUNT];
static unsigned element_count;
static uint64_t current_tree;
static HWND tree_window;
static void tree_clear(void){unsigned i;for(i=0;i<element_count;++i)if(elements[i])IUIAutomationElement_Release(elements[i]);element_count=0;current_tree=0;tree_window=NULL;}
static void bstr_json(JOut *out,BSTR value){
    char text[4097];int count;UINT length=value?SysStringLen(value):0;UINT i;
    if(length>1024)length=1024;
    count=length?WideCharToMultiByte(CP_UTF8,0,value,(int)length,text,4096,NULL,NULL):0;
    if(count<0||count>4096)count=0;
    for(i=0;i<(UINT)count;++i)if((unsigned char)text[i]<32&&text[i]!='\n'&&text[i]!='\t')text[i]=' ';
    text[count]=0;jquote(out,text);
}
static const char *control_type(int type){
    static const char *types[]={"Button","Calendar","CheckBox","ComboBox","Edit","Hyperlink","Image","ListItem","List","Menu","MenuBar","MenuItem","ProgressBar","RadioButton","ScrollBar","Slider","Spinner","StatusBar","Tab","TabItem","Text","ToolBar","ToolTip","Tree","TreeItem","Custom","Group","Thumb","DataGrid","DataItem","Document","SplitButton","Window","Pane","Header","HeaderItem","Table","TitleBar","Separator"};
    return type>=30000&&type<30000+(int)(sizeof(types)/sizeof(types[0]))?types[type-30000]:"Custom";
}
static int32_t dimension(LONG left,LONG right){int64_t n=(int64_t)right-left;return n<0?0:(n>INT32_MAX?INT32_MAX:(int32_t)n);}
static int tree_node(JOut *out,IUIAutomationElement *element,int parent,unsigned depth,unsigned max_depth,unsigned maximum,int *truncated){
    unsigned id=element_count;BSTR text=NULL;CONTROLTYPEID type=0;BOOL yes=FALSE,password=FALSE;RECT rectangle={0};IUIAutomationElement *child=NULL;
    if(id>=maximum||out->length>FRAME-16384){*truncated=1;return 0;}
    IUIAutomationElement_AddRef(element);elements[element_count++]=element;
    if(id)jtext(out,",");jtext(out,"{\"id\":");juint(out,id);jtext(out,",\"parent\":");if(parent<0)jtext(out,"null");else juint(out,(unsigned)parent);
    IUIAutomationElement_get_CurrentControlType(element,&type);jtext(out,",\"control_type\":");jquote(out,control_type(type));
    jtext(out,",\"name\":");IUIAutomationElement_get_CurrentName(element,&text);bstr_json(out,text);SysFreeString(text);text=NULL;
    jtext(out,",\"automation_id\":");IUIAutomationElement_get_CurrentAutomationId(element,&text);bstr_json(out,text);SysFreeString(text);text=NULL;
    IUIAutomationElement_get_CurrentIsPassword(element,&password);jtext(out,",\"value\":");
    if(!password){IUIAutomationValuePattern *pattern=NULL;
      if(SUCCEEDED(IUIAutomationElement_GetCurrentPatternAs(element,UIA_ValuePatternId,&IID_IUIAutomationValuePattern,(void **)&pattern))&&pattern){
        IUIAutomationValuePattern_get_CurrentValue(pattern,&text);bstr_json(out,text);SysFreeString(text);IUIAutomationValuePattern_Release(pattern);
      }else jtext(out,"null");
    }else jtext(out,"null");
    IUIAutomationElement_get_CurrentIsEnabled(element,&yes);jtext(out,yes?",\"enabled\":true":",\"enabled\":false");
    yes=FALSE;IUIAutomationElement_get_CurrentHasKeyboardFocus(element,&yes);jtext(out,yes?",\"focused\":true":",\"focused\":false");
    IUIAutomationElement_get_CurrentBoundingRectangle(element,&rectangle);jtext(out,",\"rect\":[");jint(out,rectangle.left);jtext(out,",");jint(out,rectangle.top);jtext(out,",");jint(out,dimension(rectangle.left,rectangle.right));jtext(out,",");jint(out,dimension(rectangle.top,rectangle.bottom));jtext(out,"]}");
    if(!out->ok){*truncated=1;return 0;}
    if(SUCCEEDED(IUIAutomationTreeWalker_GetFirstChildElement(walker,element,&child))&&child){
        if(depth>=max_depth){*truncated=1;IUIAutomationElement_Release(child);return 1;}
        while(child){IUIAutomationElement *next=NULL;
            if(element_count>=maximum){*truncated=1;IUIAutomationElement_Release(child);break;}
            if(!tree_node(out,child,(int)id,depth+1,max_depth,maximum,truncated)){IUIAutomationElement_Release(child);break;}
            IUIAutomationTreeWalker_GetNextSiblingElement(walker,child,&next);IUIAutomationElement_Release(child);child=next;
        }
    }
    return out->ok;
}
static void ui_tree(JOut *out,JDoc *doc,int command,uint64_t id){
    IUIAutomationElement *root=NULL;unsigned depth=(unsigned)jnumber(doc,jget(doc,command,"max_depth"),0),maximum=(unsigned)jnumber(doc,jget(doc,command,"max_nodes"),0);int truncated=0;
    tree_clear();
    if(!automation||!walker){error_reply(out,"ui_unavailable");return;}
    if(!depth||depth>32||!maximum||maximum>NODE_COUNT){error_reply(out,"tree_limit");return;}
    tree_window=GetForegroundWindow();
    if(!tree_window||FAILED(IUIAutomation_ElementFromHandle(automation,tree_window,&root))||!root){error_reply(out,"window_unavailable");return;}
    current_tree=id;jtext(out,"{\"result\":\"tree\",\"tree\":");juint(out,id);jtext(out,",\"nodes\":[");
    tree_node(out,root,-1,0,depth,maximum,&truncated);IUIAutomationElement_Release(root);
    jtext(out,truncated?"],\"truncated\":true}":"],\"truncated\":false}");
}
static void ui_invoke(JOut *out,JDoc *doc,int command){
    unsigned id=(unsigned)jnumber(doc,jget(doc,command,"element"),NODE_COUNT);HRESULT hr=E_FAIL;int action=jget(doc,command,"action");
    if(jnumber(doc,jget(doc,command,"tree"),0)!=current_tree||!current_tree||id>=element_count||GetForegroundWindow()!=tree_window){error_reply(out,"stale_element");return;}
    if(jequal(doc,action,"invoke")){IUIAutomationInvokePattern *pattern=NULL;
        if(SUCCEEDED(IUIAutomationElement_GetCurrentPatternAs(elements[id],UIA_InvokePatternId,&IID_IUIAutomationInvokePattern,(void **)&pattern))&&pattern){hr=IUIAutomationInvokePattern_Invoke(pattern);IUIAutomationInvokePattern_Release(pattern);}
    }else if(jequal(doc,action,"select")){IUIAutomationSelectionItemPattern *pattern=NULL;
        if(SUCCEEDED(IUIAutomationElement_GetCurrentPatternAs(elements[id],UIA_SelectionItemPatternId,&IID_IUIAutomationSelectionItemPattern,(void **)&pattern))&&pattern){hr=IUIAutomationSelectionItemPattern_Select(pattern);IUIAutomationSelectionItemPattern_Release(pattern);}
    }else if(jequal(doc,action,"set_value")){
        char *text=alloc(16385);WCHAR *value=alloc(32770);IUIAutomationValuePattern *pattern=NULL;
        if(text&&value&&jstring(doc,jget(doc,command,"value"),text,16385)&&wide(text,value,16385)
            &&SUCCEEDED(IUIAutomationElement_GetCurrentPatternAs(elements[id],UIA_ValuePatternId,&IID_IUIAutomationValuePattern,(void **)&pattern))&&pattern){hr=IUIAutomationValuePattern_SetValue(pattern,value);IUIAutomationValuePattern_Release(pattern);}
        release(text,16385);release(value,32770);
    }
    if(SUCCEEDED(hr))jtext(out,"{\"result\":\"ok\"}");else error_reply(out,"action_refused");
}
static void clipboard_read(JOut *out){
    HANDLE data;const WCHAR *text;char *utf8=NULL;SIZE_T size;int count;size_t start=out->length;
    if(!OpenClipboard(NULL)){error_reply(out,"clipboard_busy");return;}
    data=GetClipboardData(CF_UNICODETEXT);size=data?GlobalSize(data):0;
    if(!data){jtext(out,"{\"result\":\"clipboard\",\"text\":\"\"}");CloseClipboard();return;}
    if(!size||size>128*1024){error_reply(out,"clipboard_limit");CloseClipboard();return;}
    text=GlobalLock(data);
    if(text){size_t n=0;while(n<size/sizeof(WCHAR)&&text[n])++n;
      if(n<size/sizeof(WCHAR)){count=WideCharToMultiByte(CP_UTF8,0,text,(int)n,NULL,0,NULL,NULL);
        if(count>=0&&count<=65536&&(utf8=alloc((size_t)count+1))){WideCharToMultiByte(CP_UTF8,0,text,(int)n,utf8,count,NULL,NULL);utf8[count]=0;jtext(out,"{\"result\":\"clipboard\",\"text\":");jquote(out,utf8);jtext(out,"}");release(utf8,(size_t)count+1);}
      }
      GlobalUnlock(data);
    }
    CloseClipboard();if(out->length==start)error_reply(out,"clipboard_limit");
}
static void clipboard_write(JOut *out,JDoc *doc,int command){
    char *text=alloc(65537);WCHAR *value=alloc(131074);HGLOBAL data=NULL;void *target;int count=0,ok=0;
    if(text&&value&&jstring(doc,jget(doc,command,"text"),text,65537)&&wide(text,value,65537)){
        count=(int)wlength(value)+1;data=GlobalAlloc(GMEM_MOVEABLE,(SIZE_T)count*sizeof(WCHAR));
        if(data&&(target=GlobalLock(data))){memcpy(target,value,(size_t)count*sizeof(WCHAR));GlobalUnlock(data);
            if(OpenClipboard(NULL)){if(EmptyClipboard()&&SetClipboardData(CF_UNICODETEXT,data)){ok=1;data=NULL;}CloseClipboard();}
        }
    }
    if(data)GlobalFree(data);release(text,65537);release(value,131074);
    if(ok)jtext(out,"{\"result\":\"ok\"}");else error_reply(out,"clipboard_refused");
}

typedef struct {HANDLE file;HCRYPTHASH hash;WCHAR stage[PATH_CAP],target[PATH_CAP];char id[65],digest[65];uint64_t size,written;} Transfer;
static Transfer transfers[8];
static void hex_encode(const BYTE *bytes,size_t count,char *out){static const char h[]="0123456789abcdef";size_t i;for(i=0;i<count;++i){out[2*i]=h[bytes[i]>>4];out[2*i+1]=h[bytes[i]&15];}out[count*2]=0;}
static int hex_value(char c){if(c>='0'&&c<='9')return c-'0';if(c>='a'&&c<='f')return c-'a'+10;return -1;}
static int hex_decode(const char *text,BYTE *out,size_t capacity,size_t *count){size_t n=strlen(text),i;if((n&1)||n/2>capacity)return 0;for(i=0;i<n;i+=2){int a=hex_value(text[i]),b=hex_value(text[i+1]);if(a<0||b<0)return 0;out[i/2]=(BYTE)(a*16+b);}*count=n/2;return 1;}
static void transfer_cancel(Transfer *t){if(t->file&&t->file!=INVALID_HANDLE_VALUE)CloseHandle(t->file);if(t->hash)CryptDestroyHash(t->hash);if(t->stage[0])DeleteFileW(t->stage);SecureZeroMemory(t,sizeof(*t));}
static int guest_path(JDoc *doc,int request,WCHAR *output){
    int array=jget(doc,request,"path"),share=jget(doc,request,"share");size_t i;char name[256];WCHAR part[256];unsigned count=0;
    if(!jequal(doc,share,"inbox")&&!jequal(doc,share,"outbox"))return 0;
    lstrcpyW(output,guest_root);if(!wappend(output,PATH_CAP,jequal(doc,share,"inbox")?L"\\inbox":L"\\outbox"))return 0;
    if(array<0||doc->tokens[array].kind!=J_ARRAY)return 0;
    for(i=(size_t)array+1;i<doc->count&&doc->tokens[i].start<doc->tokens[array].end;++i)if(doc->tokens[i].parent==array){
        size_t n;DWORD attributes;
        if(++count>64||!jstring(doc,(int)i,name,sizeof(name))||!name[0]||equal(name,".")||equal(name,".."))return 0;
        for(n=0;name[n];++n)if((unsigned char)name[n]<32||name[n]=='/'||name[n]=='\\'||name[n]==':'||name[n]=='*'||name[n]=='?'||name[n]=='"'||name[n]=='<'||name[n]=='>'||name[n]=='|')return 0;
        if(name[n-1]=='.'||name[n-1]==' '||!wide(name,part,256)||!wappend(output,PATH_CAP,L"\\")||!wappend(output,PATH_CAP,part))return 0;
        attributes=GetFileAttributesW(output);if(attributes!=INVALID_FILE_ATTRIBUTES&&(attributes&FILE_ATTRIBUTE_REPARSE_POINT))return 0;
    }
    return 1;
}
static void guest_file(JOut *out,JDoc *doc,int request){
    size_t reply_start=out->length;
    int op=jget(doc,request,"op");WCHAR *path=alloc(PATH_CAP*sizeof(WCHAR));BYTE *bytes=alloc(CHUNK);char *hex=alloc(CHUNK*2+1);unsigned i;char ident[65];
    if(!path||!bytes||!hex){error_reply(out,"resource_limit");goto done;}
    if(jequal(doc,op,"write")||jequal(doc,op,"commit")||jequal(doc,op,"cancel")){
        Transfer *t=NULL;if(!jstring(doc,jget(doc,request,"transfer"),ident,65))goto bad;
        for(i=0;i<8;++i)if(transfers[i].file&&equal(transfers[i].id,ident)){t=&transfers[i];break;}
        if(!t)goto bad;
        if(jequal(doc,op,"write")){size_t count;DWORD wrote;
            if(!jstring(doc,jget(doc,request,"data"),hex,CHUNK*2+1)||!hex_decode(hex,bytes,CHUNK,&count)||!count
                ||jnumber(doc,jget(doc,request,"offset"),UINT64_MAX)!=t->written||count>t->size-t->written
                ||!WriteFile(t->file,bytes,(DWORD)count,&wrote,NULL)||wrote!=count||!CryptHashData(t->hash,bytes,(DWORD)count,0)){transfer_cancel(t);goto bad;}
            t->written+=count;
        }else if(jequal(doc,op,"commit")){DWORD n=32;BYTE digest[32];char expected[65];
            if(t->written!=t->size||!CryptGetHashParam(t->hash,HP_HASHVAL,digest,&n,0)||n!=32){transfer_cancel(t);goto bad;}
            hex_encode(digest,32,expected);
            if(!equal(expected,t->digest)||!FlushFileBuffers(t->file)){transfer_cancel(t);goto bad;}
            CloseHandle(t->file);t->file=NULL;
            if(!MoveFileW(t->stage,t->target)){transfer_cancel(t);goto bad;}
            t->stage[0]=0;transfer_cancel(t);
        }else transfer_cancel(t);
        jtext(out,"{\"result\":\"ok\"}");goto done;
    }
    if(!guest_path(doc,request,path))goto bad;
    if(jequal(doc,op,"read")){HANDLE file;LARGE_INTEGER size,offset;DWORD count;uint64_t length=jnumber(doc,jget(doc,request,"length"),0);
        if(!length||length>CHUNK)goto bad;
        file=CreateFileW(path,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,FILE_FLAG_OPEN_REPARSE_POINT,NULL);if(file==INVALID_HANDLE_VALUE)goto bad;
        offset.QuadPart=(LONGLONG)jnumber(doc,jget(doc,request,"offset"),UINT64_MAX);
        if(!GetFileSizeEx(file,&size)||size.QuadPart<0||(uint64_t)size.QuadPart>4ULL*1024*1024*1024||offset.QuadPart<0||offset.QuadPart>size.QuadPart||!SetFilePointerEx(file,offset,NULL,FILE_BEGIN)||!ReadFile(file,bytes,(DWORD)length,&count,NULL)){CloseHandle(file);goto bad;}
        CloseHandle(file);hex_encode(bytes,count,hex);jtext(out,"{\"result\":\"data\",\"offset\":");juint(out,(uint64_t)offset.QuadPart);jtext(out,",\"data\":");jquote(out,hex);jtext(out,(uint64_t)offset.QuadPart+count>=(uint64_t)size.QuadPart?",\"eof\":true}":",\"eof\":false}");goto done;
    }
    if(jequal(doc,op,"begin")){Transfer *t=NULL;BYTE nonce[32];WCHAR suffix[80];size_t n;uint64_t size=jnumber(doc,jget(doc,request,"size"),UINT64_MAX);
        for(i=0;i<8;++i)if(!transfers[i].file){t=&transfers[i];break;}
        if(!t||size>4ULL*1024*1024*1024||!crypto||!jstring(doc,jget(doc,request,"sha256"),t->digest,65)||strlen(t->digest)!=64||!CryptGenRandom(crypto,32,nonce))goto bad;
        hex_encode(nonce,32,t->id);lstrcpyW(t->target,path);lstrcpyW(t->stage,path);
        n=wlength(t->stage);while(n&&t->stage[n-1]!=L'\\')--n;t->stage[n]=0;
        if(!wide(t->id,suffix,80)||!wappend(t->stage,PATH_CAP,L".lightvm-transfer-")||!wappend(t->stage,PATH_CAP,suffix)){transfer_cancel(t);goto bad;}
        t->file=CreateFileW(t->stage,GENERIC_WRITE,0,NULL,CREATE_NEW,FILE_ATTRIBUTE_NORMAL,NULL);
        if(t->file==INVALID_HANDLE_VALUE||!CryptCreateHash(crypto,CALG_SHA_256,0,0,&t->hash)){transfer_cancel(t);goto bad;}
        t->size=size;t->written=0;jtext(out,"{\"result\":\"transfer\",\"transfer\":");jquote(out,t->id);jtext(out,"}");goto done;
    }
    if(jequal(doc,op,"list")){WIN32_FIND_DATAW item;HANDLE search;int first=1;unsigned count=0;char name[1024];
        if(!wappend(path,PATH_CAP,L"\\*"))goto bad;
        search=FindFirstFileW(path,&item);if(search==INVALID_HANDLE_VALUE)goto bad;
        jtext(out,"{\"result\":\"entries\",\"entries\":[");
        do{if(item.cFileName[0]==L'.'&&(item.cFileName[1]==0||(item.cFileName[1]==L'.'&&item.cFileName[2]==0)))continue;
            if(item.dwFileAttributes&FILE_ATTRIBUTE_REPARSE_POINT)continue;
            if(++count>512){FindClose(search);goto bad_reset;}
            if(!WideCharToMultiByte(CP_UTF8,0,item.cFileName,-1,name,sizeof(name),NULL,NULL))continue;
            if(starts(name,".lightvm-transfer-"))continue;
            if(!first)jtext(out,",");first=0;jtext(out,"{\"name\":");jquote(out,name);jtext(out,(item.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY)?",\"directory\":true,\"size\":":",\"directory\":false,\"size\":");juint(out,((uint64_t)item.nFileSizeHigh<<32)|item.nFileSizeLow);jtext(out,"}");
        }while(FindNextFileW(search,&item));FindClose(search);jtext(out,"]}");goto done;
    }
bad:error_reply(out,"file_operation_refused");goto done;
bad_reset:out->length=reply_start;out->ok=1;error_reply(out,"directory_limit");
done:release(path,PATH_CAP*sizeof(WCHAR));release(bytes,CHUNK);release(hex,CHUNK*2+1);
}

static DWORD WINAPI control_thread(void *unused){
    Channel c;unsigned i;(void)unused;if(!channel_init(&c))return 1;
    CoInitializeEx(NULL,COINIT_MULTITHREADED);
    if(SUCCEEDED(CoCreateInstance(&CLSID_CUIAutomation,NULL,CLSCTX_INPROC_SERVER,&IID_IUIAutomation,(void **)&automation))&&automation)IUIAutomation_get_ControlViewWalker(automation,&walker);
    while(InterlockedCompareExchange(&running,1,1)){
        if(!channel_connect(&c,1)){Sleep(1000);continue;}
        while(frame_receive(&c)){
            JOut out;int command=jget(&c.doc,0,"command"),op=jget(&c.doc,command,"op");uint64_t id=jnumber(&c.doc,jget(&c.doc,0,"id"),0);size_t reply_start;
            jout(&out,c.output,FRAME);jtext(&out,"{\"id\":");juint(&out,id);jtext(&out,",\"reply\":");reply_start=out.length;
            if(jequal(&c.doc,op,"clipboard_read"))clipboard_read(&out);
            else if(jequal(&c.doc,op,"clipboard_write"))clipboard_write(&out,&c.doc,command);
            else if(jequal(&c.doc,op,"ui_tree"))ui_tree(&out,&c.doc,command,id);
            else if(jequal(&c.doc,op,"ui_invoke"))ui_invoke(&out,&c.doc,command);
            else if(jequal(&c.doc,op,"file")){jtext(&out,"{\"result\":\"file\",\"reply\":");guest_file(&out,&c.doc,jget(&c.doc,command,"request"));jtext(&out,"}");}
            else if(jequal(&c.doc,op,"flush")){for(i=0;i<8;++i)if(transfers[i].file)FlushFileBuffers(transfers[i].file);jtext(&out,"{\"result\":\"ok\"}");}
            else error_reply(&out,"unsupported");
            if(!out.ok){out.length=reply_start;out.ok=1;error_reply(&out,"reply_limit");tree_clear();}
            jtext(&out,"}");if(!frame_send(&c,&out))break;
        }
        channel_close(&c);tree_clear();for(i=0;i<8;++i)if(transfers[i].file)transfer_cancel(&transfers[i]);Sleep(500);
    }
    return 0;
}

/* Local HTTP/WebDAV front end. Network requests never carry host paths;
 * every operation goes back through the authenticated supervisor channel. */
static Channel files;
static int request_path(const char *url,char *share,char parts[64][256],unsigned *count){
    char decoded[4097];size_t i=0,n=0,start;unsigned segment=0;
    while(url[i]&&url[i]!='?'&&n<sizeof(decoded)-1){char c=url[i++];if(c=='%'){int a,b;if(!url[i]||!url[i+1])return 0;a=hex_value(url[i]);b=hex_value(url[i+1]);if(a<0||b<0)return 0;c=(char)(a*16+b);i+=2;}if(!c||(unsigned char)c<32||c=='\\')return 0;decoded[n++]=c;}decoded[n]=0;
    i=0;while(i<n){while(i<n&&decoded[i]=='/')++i;if(i==n)break;start=i;while(i<n&&decoded[i]!='/')++i;
        if(i-start>255||segment>=65)return 0;
        if(segment==0){size_t j;if(i-start>48)return 0;for(j=0;j<i-start;++j)share[j]=decoded[start+j];share[i-start]=0;
            if(equal(share,"DavWWWRoot")){segment=0;continue;}
        }else{size_t j;for(j=0;j<i-start;++j)parts[segment-1][j]=decoded[start+j];parts[segment-1][i-start]=0;if(equal(parts[segment-1],".")||equal(parts[segment-1],".."))return 0;}
        ++segment;
    }
    *count=segment?segment-1:0;
    if(!segment){share[0]=0;return 1;}
    for(i=0;i<bootstrap.share_count;++i)if(equal(share,bootstrap.shares[i]))return 1;
    return 0;
}
static void path_json(JOut *out,const char *share,char parts[64][256],unsigned count){unsigned i;jtext(out,",\"share\":");jquote(out,share);jtext(out,",\"path\":[");for(i=0;i<count;++i){if(i)jtext(out,",");jquote(out,parts[i]);}jtext(out,"]");}
static void http_reply(SOCKET socket,unsigned code,const char *type,const char *body,size_t size,int head){
    char header[1024];JOut out;jout(&out,header,sizeof(header));jtext(&out,"HTTP/1.1 ");juint(&out,code);jtext(&out," LightVM\r\nConnection: close\r\nContent-Length: ");juint(&out,size);jtext(&out,"\r\nContent-Type: ");jtext(&out,type);jtext(&out,"\r\nDAV: 1,2\r\n\r\n");
    io_all(socket,header,out.length,1);if(!head&&size)io_all(socket,(char *)body,size,1);
}
static int header_value(const char *headers,const char *key,char *output,size_t capacity){
    const char *line=headers;size_t n=strlen(key),i;
    while(*line){const char *end=line;while(*end&&*end!='\r'&&*end!='\n')++end;
        for(i=0;i<n&&line+i<end;++i){char a=line[i],b=key[i];if(a>='A'&&a<='Z')a+=32;if(b>='A'&&b<='Z')b+=32;if(a!=b)break;}
        if(i==n&&line[n]==':'){const char *value=line+n+1;size_t size;while(*value==' '||*value=='\t')++value;size=(size_t)(end-value);if(size>=capacity)return 0;memcpy(output,value,size);output[size]=0;return 1;}
        while(*end=='\r'||*end=='\n')++end;line=end;
    }
    return 0;
}
static uint64_t decimal(const char *s,uint64_t fallback){uint64_t n=0;if(!*s)return fallback;while(*s){unsigned c=(unsigned char)*s++;if(c<'0'||c>'9'||n>(UINT64_MAX-(c-'0'))/10)return fallback;n=n*10+c-'0';}return n;}
static void xml_text(JOut *out,const char *s){for(;*s;++s){if(*s=='&')jtext(out,"&amp;");else if(*s=='<')jtext(out,"&lt;");else if(*s=='>')jtext(out,"&gt;");else if(*s=='"')jtext(out,"&quot;");else jraw(out,s,1);}}
static void dav_entry(JOut *out,const char *href,const char *name,int directory,uint64_t size){
    jtext(out,"<D:response><D:href>");xml_text(out,href);jtext(out,"</D:href><D:propstat><D:prop><D:displayname>");xml_text(out,name);jtext(out,"</D:displayname><D:resourcetype>");if(directory)jtext(out,"<D:collection/>");jtext(out,"</D:resourcetype><D:getcontentlength>");juint(out,size);jtext(out,"</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>");
}
static void http_request(SOCKET socket){
    char *header=alloc(32769),*body=alloc(FRAME),*share=alloc(49),(*parts)[256]=alloc(64*256);char method[16],url[4097];size_t used=0,i=0,j=0;unsigned count=0;JOut request,out;DWORD timeout=15000;
    if(!header||!body||!share||!parts)goto done;
    setsockopt(socket,SOL_SOCKET,SO_RCVTIMEO,(const char *)&timeout,sizeof(timeout));setsockopt(socket,SOL_SOCKET,SO_SNDTIMEO,(const char *)&timeout,sizeof(timeout));
    while(used<32768){if(!io_all(socket,header+used,1,0))goto done;++used;if(used>=4&&header[used-4]=='\r'&&header[used-3]=='\n'&&header[used-2]=='\r'&&header[used-1]=='\n')break;}
    header[used]=0;if(used>=32768)goto done;
    while(header[i]&&header[i]!=' '&&j<15)method[j++]=header[i++];method[j]=0;if(header[i++]!=' ')goto done;
    j=0;while(header[i]&&header[i]!=' '&&j<4096)url[j++]=header[i++];url[j]=0;if(header[i]!=' '||!request_path(url,share,parts,&count))goto denied;
    if(equal(method,"OPTIONS")){const char response[]="HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\nDAV: 1,2\r\nAllow: OPTIONS, PROPFIND, GET, HEAD, PUT, MKCOL, DELETE, MOVE, LOCK, UNLOCK, PROPPATCH\r\n\r\n";io_all(socket,(char *)response,sizeof(response)-1,1);goto done;}
    EnterCriticalSection(&file_lock);
    if(equal(method,"PROPFIND")){
        int entries;size_t token;char name[1024],*href=alloc(4097),depth[16]={0};
        if(!href){LeaveCriticalSection(&file_lock);goto denied;}
        if(header_value(header,"Depth",depth,sizeof(depth))&&!equal(depth,"0")&&!equal(depth,"1")){release(href,4097);LeaveCriticalSection(&file_lock);goto denied;}
        jout(&out,body,FRAME);jtext(&out,"<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">");
        if(!share[0]){dav_entry(&out,"/","LightVM",1,0);for(i=0;i<bootstrap.share_count;++i){JOut h;jout(&h,href,4096);jtext(&h,"/");jtext(&h,bootstrap.shares[i]);href[h.length]=0;dav_entry(&out,href,bootstrap.shares[i],1,0);}}
        else{
            jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"list\"");path_json(&request,share,parts,count);jtext(&request,"}");
            if(!file_call(&files,&request)){release(href,4097);LeaveCriticalSection(&file_lock);goto denied;}
            if(!result_is(&files,"entries")&&count){
                int found=0;
                jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"list\"");path_json(&request,share,parts,count-1);jtext(&request,"}");
                if(file_call(&files,&request)&&result_is(&files,"entries")){
                    entries=jget(&files.doc,0,"entries");
                    for(token=(size_t)(entries+1);entries>=0&&token<files.doc.count&&files.doc.tokens[token].start<files.doc.tokens[entries].end;++token)if(files.doc.tokens[token].parent==entries&&files.doc.tokens[token].kind==J_OBJECT){
                        if(jstring(&files.doc,jget(&files.doc,(int)token,"name"),name,sizeof(name))&&equal(name,parts[count-1])){
                            dav_entry(&out,url,name,token_kind(&files.doc,jget(&files.doc,(int)token,"directory"))==J_TRUE,jnumber(&files.doc,jget(&files.doc,(int)token,"size"),0));found=1;break;
                        }
                    }
                }
                if(!found){release(href,4097);LeaveCriticalSection(&file_lock);http_reply(socket,404,"text/plain","",0,0);goto done;}
                jtext(&out,"</D:multistatus>");release(href,4097);LeaveCriticalSection(&file_lock);http_reply(socket,207,"application/xml; charset=utf-8",body,out.length,0);goto done;
            }
            if(!result_is(&files,"entries")){release(href,4097);LeaveCriticalSection(&file_lock);goto denied;}
            dav_entry(&out,url,count?parts[count-1]:share,1,0);
            entries=jget(&files.doc,0,"entries");
            for(token=(size_t)(entries+1);entries>=0&&token<files.doc.count&&files.doc.tokens[token].start<files.doc.tokens[entries].end;++token)if(files.doc.tokens[token].parent==entries&&files.doc.tokens[token].kind==J_OBJECT){
                JOut h;int directory=token_kind(&files.doc,jget(&files.doc,(int)token,"directory"))==J_TRUE;
                if(equal(depth,"0"))break;
                if(!jstring(&files.doc,jget(&files.doc,(int)token,"name"),name,sizeof(name)))continue;
                jout(&h,href,4096);jtext(&h,url);if(!h.length||href[h.length-1]!='/')jtext(&h,"/");jtext(&h,name);if(!h.ok)continue;href[h.length]=0;
                dav_entry(&out,href,name,directory,jnumber(&files.doc,jget(&files.doc,(int)token,"size"),0));
            }
        }
        jtext(&out,"</D:multistatus>");release(href,4097);LeaveCriticalSection(&file_lock);if(out.ok)http_reply(socket,207,"application/xml; charset=utf-8",body,out.length,0);else goto denied;goto done;
    }
    if(equal(method,"GET")||equal(method,"HEAD")){
        uint64_t size=0,offset=0;int array,found=0;size_t token;char name[1024];
        if(!count){LeaveCriticalSection(&file_lock);goto denied;}
        jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"list\"");path_json(&request,share,parts,count-1);jtext(&request,"}");
        if(!file_call(&files,&request)||!result_is(&files,"entries")){LeaveCriticalSection(&file_lock);goto denied;}
        array=jget(&files.doc,0,"entries");for(token=(size_t)(array+1);array>=0&&token<files.doc.count&&files.doc.tokens[token].start<files.doc.tokens[array].end;++token)if(files.doc.tokens[token].parent==array&&files.doc.tokens[token].kind==J_OBJECT){
            if(jstring(&files.doc,jget(&files.doc,(int)token,"name"),name,sizeof(name))&&equal(name,parts[count-1])){size=jnumber(&files.doc,jget(&files.doc,(int)token,"size"),0);found=1;break;}
        }
        if(!found){LeaveCriticalSection(&file_lock);http_reply(socket,404,"text/plain","",0,0);goto done;}
        /* Send the declared length without allocating the entire file. */
        jout(&out,body,FRAME);jtext(&out,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: ");juint(&out,size);jtext(&out,"\r\n\r\n");io_all(socket,body,out.length,1);
        while(offset<size&&!equal(method,"HEAD")){char *hex;BYTE *data;size_t bytes;
            jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"read\"");path_json(&request,share,parts,count);jtext(&request,",\"offset\":");juint(&request,offset);jtext(&request,",\"length\":65536}");
            if(!file_call(&files,&request)||!result_is(&files,"data"))break;
            hex=alloc(CHUNK*2+1);data=alloc(CHUNK);if(!hex||!data){release(hex,CHUNK*2+1);release(data,CHUNK);break;}
            if(!jstring(&files.doc,jget(&files.doc,0,"data"),hex,CHUNK*2+1)||!hex_decode(hex,data,CHUNK,&bytes)||!bytes||!io_all(socket,(char *)data,bytes,1)){release(hex,CHUNK*2+1);release(data,CHUNK);break;}
            offset+=bytes;release(hex,CHUNK*2+1);release(data,CHUNK);
        }
        LeaveCriticalSection(&file_lock);goto done;
    }
    if(equal(method,"MKCOL")||equal(method,"DELETE")){
        int ok;jout(&request,files.output,FRAME);jtext(&request,equal(method,"MKCOL")?"{\"op\":\"mkdir\"":"{\"op\":\"remove\"");path_json(&request,share,parts,count);jtext(&request,"}");ok=file_call(&files,&request)&&result_is(&files,"ok");LeaveCriticalSection(&file_lock);http_reply(socket,ok?(equal(method,"MKCOL")?201:204):403,"text/plain","",0,0);goto done;
    }
    if(equal(method,"PUT")){
        char value[64],transfer[65]={0},digest_hex[65];BYTE digest[32],nonce[16];WCHAR temporary[PATH_CAP],suffix[65];
        HANDLE spool=INVALID_HANDLE_VALUE;HCRYPTHASH hash=0;uint64_t size,remaining,offset=0;DWORD wrote,n=32;BYTE *chunk=alloc(CHUNK);char *encoded=alloc(CHUNK*2+1);int ok=0;
        if(!chunk||!encoded||!count||!crypto||!header_value(header,"Content-Length",value,sizeof(value)))goto put_done;
        size=decimal(value,UINT64_MAX);if(size>4ULL*1024*1024*1024)goto put_done;
        if(header_value(header,"Expect",value,sizeof(value))&&equal(value,"100-continue")){const char ready[]="HTTP/1.1 100 Continue\r\n\r\n";if(!io_all(socket,(char *)ready,sizeof(ready)-1,1))goto put_done;}
        if(!CryptGenRandom(crypto,16,nonce)||!CryptCreateHash(crypto,CALG_SHA_256,0,0,&hash))goto put_done;
        hex_encode(nonce,16,digest_hex);if(!wide(digest_hex,suffix,65))goto put_done;
        lstrcpyW(temporary,guest_root);if(!wappend(temporary,PATH_CAP,L"\\.webdav-")||!wappend(temporary,PATH_CAP,suffix))goto put_done;
        spool=CreateFileW(temporary,GENERIC_READ|GENERIC_WRITE,0,NULL,CREATE_NEW,FILE_ATTRIBUTE_TEMPORARY|FILE_FLAG_DELETE_ON_CLOSE,NULL);if(spool==INVALID_HANDLE_VALUE)goto put_done;
        remaining=size;
        while(remaining){DWORD bytes=(DWORD)(remaining>CHUNK?CHUNK:remaining);
            if(!io_all(socket,(char *)chunk,bytes,0)||!WriteFile(spool,chunk,bytes,&wrote,NULL)||wrote!=bytes||!CryptHashData(hash,chunk,bytes,0))goto put_done;
            remaining-=bytes;
        }
        if(!CryptGetHashParam(hash,HP_HASHVAL,digest,&n,0)||n!=32)goto put_done;hex_encode(digest,32,digest_hex);
        jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"begin\",\"replace\":true");path_json(&request,share,parts,count);jtext(&request,",\"size\":");juint(&request,size);jtext(&request,",\"sha256\":");jquote(&request,digest_hex);jtext(&request,"}");
        if(!file_call(&files,&request)||!result_is(&files,"transfer")||!jstring(&files.doc,jget(&files.doc,0,"transfer"),transfer,sizeof(transfer)))goto put_done;
        {LARGE_INTEGER zero;zero.QuadPart=0;if(!SetFilePointerEx(spool,zero,NULL,FILE_BEGIN))goto put_done;}
        while(offset<size){DWORD bytes=(DWORD)((size-offset)>CHUNK?CHUNK:(size-offset)),read=0;
            if(!ReadFile(spool,chunk,bytes,&read,NULL)||read!=bytes)goto put_done;hex_encode(chunk,bytes,encoded);
            jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"write\",\"transfer\":");jquote(&request,transfer);jtext(&request,",\"offset\":");juint(&request,offset);jtext(&request,",\"data\":");jquote(&request,encoded);jtext(&request,"}");
            if(!file_call(&files,&request)||!result_is(&files,"ok"))goto put_done;offset+=bytes;
        }
        jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"commit\",\"transfer\":");jquote(&request,transfer);jtext(&request,"}");ok=file_call(&files,&request)&&result_is(&files,"ok");
put_done:
        if(!ok&&transfer[0]){jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"cancel\",\"transfer\":");jquote(&request,transfer);jtext(&request,"}");file_call(&files,&request);}
        if(spool!=INVALID_HANDLE_VALUE)CloseHandle(spool);if(hash)CryptDestroyHash(hash);release(chunk,CHUNK);release(encoded,CHUNK*2+1);
        LeaveCriticalSection(&file_lock);http_reply(socket,ok?201:403,"text/plain","",0,0);goto done;
    }
    if(equal(method,"MOVE")){
        char *destination=alloc(4097),*target_share=alloc(49),(*target_parts)[256]=alloc(64*256),overwrite[8];unsigned target_count=0;int ok=0;const char *target;
        if(destination&&target_share&&target_parts&&header_value(header,"Destination",destination,4097)){
            target=destination;
            if(starts(target,"http://localhost:47081"))target+=22;
            else if(starts(target,"http://127.0.0.1:47081"))target+=22;
            if(*target=='/'&&request_path(target,target_share,target_parts,&target_count)&&equal(target_share,share)&&target_count&&count){unsigned p;
                jout(&request,files.output,FRAME);jtext(&request,"{\"op\":\"rename\",\"share\":");jquote(&request,share);jtext(&request,",\"from\":[");for(p=0;p<count;++p){if(p)jtext(&request,",");jquote(&request,parts[p]);}jtext(&request,"],\"to\":[");for(p=0;p<target_count;++p){if(p)jtext(&request,",");jquote(&request,target_parts[p]);}
                jtext(&request,header_value(header,"Overwrite",overwrite,sizeof(overwrite))&&equal(overwrite,"F")?"],\"replace\":false}":"],\"replace\":true}");ok=file_call(&files,&request)&&result_is(&files,"ok");
            }
        }
        release(destination,4097);release(target_share,49);release(target_parts,64*256);LeaveCriticalSection(&file_lock);http_reply(socket,ok?201:403,"text/plain","",0,0);goto done;
    }
    if(equal(method,"LOCK")){
        BYTE nonce[16];char token[33];JOut h;size_t body_size;
        if(!crypto||!CryptGenRandom(crypto,16,nonce)){LeaveCriticalSection(&file_lock);goto denied;}
        hex_encode(nonce,16,token);jout(&out,body,FRAME);jtext(&out,"<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock><D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope><D:depth>0</D:depth><D:timeout>Second-300</D:timeout><D:locktoken><D:href>opaquelocktoken:");jtext(&out,token);jtext(&out,"</D:href></D:locktoken></D:activelock></D:lockdiscovery></D:prop>");body_size=out.length;
        jout(&h,header,32769);jtext(&h,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/xml\r\nLock-Token: <opaquelocktoken:");jtext(&h,token);jtext(&h,">\r\nContent-Length: ");juint(&h,body_size);jtext(&h,"\r\n\r\n");io_all(socket,header,h.length,1);io_all(socket,body,body_size,1);LeaveCriticalSection(&file_lock);goto done;
    }
    if(equal(method,"UNLOCK")||equal(method,"PROPPATCH")){const char response[]="<D:multistatus xmlns:D=\"DAV:\"/>";LeaveCriticalSection(&file_lock);http_reply(socket,equal(method,"UNLOCK")?204:207,"application/xml",response,sizeof(response)-1,0);goto done;}
    /* Native file transfer remains available when a redirector cannot map
       this OS. Unimplemented HTTP verbs are never translated into host I/O. */
    LeaveCriticalSection(&file_lock);
denied:http_reply(socket,403,"text/plain","",0,0);
done:release(header,32769);release(body,FRAME);release(share,49);release(parts,64*256);
}

static DWORD WINAPI map_thread(void *unused){
    unsigned i;DWORD drives;WCHAR local[3]={L'Z',L':',0};(void)unused;Sleep(1500);drives=GetLogicalDrives();
    for(i=0;i<bootstrap.share_count;++i){NETRESOURCEW resource;WCHAR remote[128],name[49];
        while(local[0]>=L'G'&&(drives&(1u<<(local[0]-L'A'))))--local[0];if(local[0]<L'G')break;
        lstrcpyW(remote,L"\\\\localhost@47081\\DavWWWRoot\\");if(!wide(bootstrap.shares[i],name,49)||!wappend(remote,128,name))continue;
        memset(&resource,0,sizeof(resource));resource.dwType=RESOURCETYPE_DISK;resource.lpLocalName=local;resource.lpRemoteName=remote;
        if(WNetAddConnection2W(&resource,NULL,NULL,0)==NO_ERROR)drives|=1u<<(local[0]-L'A');--local[0];
    }
    return 0;
}

static void install(void){
    WCHAR *source=alloc(PATH_CAP*sizeof(WCHAR)),*folder=alloc(PATH_CAP*sizeof(WCHAR)),*target=alloc(PATH_CAP*sizeof(WCHAR)),*command=alloc(PATH_CAP*2*sizeof(WCHAR));HKEY key;DWORD limit=0xffffffffu;SC_HANDLE scm,service;
    if(!source||!folder||!target||!command)goto done;
    if(FAILED(SHGetFolderPathW(NULL,CSIDL_LOCAL_APPDATA,NULL,SHGFP_TYPE_CURRENT,folder))||!wappend(folder,PATH_CAP,L"\\LightVM Tools"))goto done;
    CreateDirectoryW(folder,NULL);lstrcpyW(target,folder);if(!wappend(target,PATH_CAP,L"\\TOOLS.EXE")||!GetModuleFileNameW(NULL,source,PATH_CAP))goto done;
    if(lstrcmpiW(source,target)!=0&&!CopyFileW(source,target,FALSE))goto done;
    lstrcpyW(command,L"\"");wappend(command,PATH_CAP*2,target);wappend(command,PATH_CAP*2,L"\" --run");
    if(RegCreateKeyExW(HKEY_CURRENT_USER,L"Software\\Microsoft\\Windows\\CurrentVersion\\Run",0,NULL,0,KEY_SET_VALUE,NULL,&key,NULL)==ERROR_SUCCESS){RegSetValueExW(key,L"LightVM Tools",0,REG_SZ,(const BYTE *)command,(DWORD)((wlength(command)+1)*sizeof(WCHAR)));RegCloseKey(key);}
    /* Explicit installer operation, inside the guest. No authentication
       setting is weakened; the guest-loopback relay authenticates upstream. */
    if(RegOpenKeyExW(HKEY_LOCAL_MACHINE,L"SYSTEM\\CurrentControlSet\\Services\\WebClient\\Parameters",0,KEY_SET_VALUE,&key)==ERROR_SUCCESS){RegSetValueExW(key,L"FileSizeLimitInBytes",0,REG_DWORD,(const BYTE *)&limit,sizeof(limit));RegCloseKey(key);}
    scm=OpenSCManagerW(NULL,NULL,SC_MANAGER_CONNECT);if(scm){service=OpenServiceW(scm,L"WebClient",SERVICE_CHANGE_CONFIG|SERVICE_START);if(service){ChangeServiceConfigW(service,SERVICE_NO_CHANGE,SERVICE_AUTO_START,SERVICE_NO_CHANGE,NULL,NULL,NULL,NULL,NULL,NULL,NULL);StartServiceW(service,0,NULL);CloseServiceHandle(service);}CloseServiceHandle(scm);}
    message(L"LightVM Tools installed for this user. Shared folders require the WebClient service; run this installer as administrator in the guest to enable it and allow large files. The Tools start when you sign in.");
done:release(source,PATH_CAP*sizeof(WCHAR));release(folder,PATH_CAP*sizeof(WCHAR));release(target,PATH_CAP*sizeof(WCHAR));release(command,PATH_CAP*2*sizeof(WCHAR));
}

void mainCRTStartup(void){
    WSADATA data;int argc=0;LPWSTR *argv=CommandLineToArgvW(GetCommandLineW(),&argc);SOCKET listener;struct sockaddr_in address;HANDLE thread;
    typedef BOOL (WINAPI *DpiAware)(void);DpiAware dpi=(DpiAware)(void *)GetProcAddress(GetModuleHandleW(L"user32.dll"),"SetProcessDPIAware");if(dpi)dpi();
    if(argc>1&&lstrcmpW(argv[1],L"--install")==0)install();if(argv)LocalFree(argv);
    while(!read_bootstrap())Sleep(2000);
    if(FAILED(SHGetFolderPathW(NULL,CSIDL_PERSONAL,NULL,SHGFP_TYPE_CURRENT,guest_root))||!wappend(guest_root,PATH_CAP,L"\\LightVM Tools"))ExitProcess(2);
    CreateDirectoryW(guest_root,NULL);{WCHAR path[PATH_CAP];lstrcpyW(path,guest_root);wappend(path,PATH_CAP,L"\\inbox");CreateDirectoryW(path,NULL);lstrcpyW(path,guest_root);wappend(path,PATH_CAP,L"\\outbox");CreateDirectoryW(path,NULL);}
    CryptAcquireContextW(&crypto,NULL,NULL,PROV_RSA_AES,CRYPT_VERIFYCONTEXT);
    if(WSAStartup(MAKEWORD(2,2),&data)!=0||!channel_init(&files))ExitProcess(3);
    InitializeCriticalSection(&file_lock);
    thread=CreateThread(NULL,0,control_thread,NULL,0,NULL);if(thread)CloseHandle(thread);
    listener=socket(AF_INET,SOCK_STREAM,IPPROTO_TCP);memset(&address,0,sizeof(address));address.sin_family=AF_INET;address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);address.sin_port=htons(47081);
    if(listener==INVALID_SOCKET||bind(listener,(struct sockaddr *)&address,sizeof(address))||listen(listener,8))ExitProcess(4);
    thread=CreateThread(NULL,0,map_thread,NULL,0,NULL);if(thread)CloseHandle(thread);
    for(;;){SOCKET client=accept(listener,NULL,NULL);if(client==INVALID_SOCKET)break;http_request(client);shutdown(client,SD_BOTH);closesocket(client);}
    ExitProcess(0);
}
