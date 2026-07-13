#import <Foundation/Foundation.h>
#import <React/RCTBridgeModule.h>

@interface ReproItRuntime : NSObject <RCTBridgeModule>
@end

@implementation ReproItRuntime
RCT_EXPORT_MODULE();
+ (BOOL)requiresMainQueueSetup { return NO; }
- (NSDictionary *)constantsToExport {
  NSString *json = NSProcessInfo.processInfo.environment[@"REPROIT_CAPSULE_JSON"];
  return json.length ? @{ @"capsuleJson": json } : @{};
}
@end
