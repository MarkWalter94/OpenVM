/* Native Win32 guest clipboard qualification on a separate, create-only
 * noninteractive window station. Never opens the user's interactive clipboard.
 * Same freestanding i686 code as TOOLS.EXE; no CRT or test libraries. */
#define mainCRTStartup tools_entry_not_called_by_test
#include "tools.c"
#undef mainCRTStartup
#include <sddl.h>

static void fail(const char *text){DWORD n,code=GetLastError();char buffer[80];JOut o;WriteFile(GetStdHandle(STD_ERROR_HANDLE),text,(DWORD)strlen(text),&n,NULL);jout(&o,buffer,sizeof(buffer));jtext(&o,"Win32 error: ");juint(&o,code);jtext(&o,"\n");WriteFile(GetStdHandle(STD_ERROR_HANDLE),buffer,(DWORD)o.length,&n,NULL);ExitProcess(1);}
#define REQUIRE(condition) do{if(!(condition))fail("clipboard test failed: " #condition "\n");}while(0)

void mainCRTStartup(void){
    HWINSTA station;HDESK desktop;PSECURITY_DESCRIPTOR sd=NULL;SECURITY_ATTRIBUTES sa;
    BYTE pixels[28]={'L','I','M','G',2,0,0,0,2,0,0,0,255,0,0,255,0,255,0,255,0,0,255,255,128,64,32,128};
    ClipboardImage source;char output[4096];JOut out;JDoc doc;JToken tokens[64];DWORD n;
    {
        HANDLE token;DWORD needed=0;TOKEN_USER *user;WCHAR *sid=NULL,sddl[256];
        REQUIRE(OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token));
        GetTokenInformation(token,TokenUser,NULL,0,&needed);REQUIRE(needed&&needed<65536);
        user=alloc(needed);REQUIRE(user);REQUIRE(GetTokenInformation(token,TokenUser,user,needed,&needed));
        REQUIRE(ConvertSidToStringSidW(user->User.Sid,&sid));lstrcpyW(sddl,L"D:P(A;;GA;;;");REQUIRE(wappend(sddl,256,sid));REQUIRE(wappend(sddl,256,L")"));
        REQUIRE(ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl,SDDL_REVISION_1,&sd,NULL));
        LocalFree(sid);release(user,needed);CloseHandle(token);
    }
    sa.nLength=sizeof(sa);sa.lpSecurityDescriptor=sd;sa.bInheritHandle=FALSE;
    {
        char suffix[32];WCHAR name[96],wide_pid[32];JOut o;
        jout(&o,suffix,sizeof(suffix)-1);juint(&o,GetCurrentProcessId());suffix[o.length]=0;REQUIRE(wide(suffix,wide_pid,32));
        lstrcpyW(name,L"LightVMToolsClipboard-");REQUIRE(wappend(name,96,wide_pid));
        station=CreateWindowStationW(name,1,GENERIC_ALL,&sa);
    }
    LocalFree(sd);
    REQUIRE(station);REQUIRE(station!=GetProcessWindowStation());REQUIRE(SetProcessWindowStation(station));
    desktop=CreateDesktopW(L"Default",NULL,NULL,0,GENERIC_ALL,NULL);REQUIRE(desktop);REQUIRE(SetThreadDesktop(desktop));
    memset(&source,0,sizeof(source));source.data=pixels;source.size=sizeof(pixels);
    REQUIRE(image_publish(&source));REQUIRE(image_capture_native(&image_capture));
    REQUIRE(image_capture.size==sizeof(pixels));
    for(n=0;n<sizeof(pixels);++n)REQUIRE(pixels[n]==image_capture.data[n]);
    image_clear(&image_capture);
    pixels[4]=0;REQUIRE(!image_publish(&source));pixels[4]=2;
    {
        const char text[]="{\"text\":\"native clipboard text\"}";
        REQUIRE(jparse(&doc,text,sizeof(text)-1,tokens,64));jout(&out,output,sizeof(output));clipboard_write(&out,&doc,0);REQUIRE(out.ok);
        REQUIRE(jparse(&doc,output,out.length,tokens,64));REQUIRE(jequal(&doc,jget(&doc,0,"result"),"ok"));
        jout(&out,output,sizeof(output));clipboard_read(&out);REQUIRE(out.ok);
        REQUIRE(jparse(&doc,output,out.length,tokens,64));REQUIRE(jequal(&doc,jget(&doc,0,"text"),"native clipboard text"));
    }
    {
        const char *done="native i686 clipboard text and RGBA round trip passed in isolated window station\n";
        WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),done,(DWORD)strlen(done),&n,NULL);
    }
    ExitProcess(0);
}
