package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	pb "google.golang.org/grpc/examples/helloworld/helloworld"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/types/descriptorpb"
)

func main() {
	output := os.Getenv("REPROIT_OSS_TMP")
	if output == "" {
		panic("REPROIT_OSS_TMP is required")
	}
	conn, err := grpc.NewClient(
		"localhost:50051",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		panic(err)
	}
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	reply, err := pb.NewGreeterClient(conn).SayHello(ctx, &pb.HelloRequest{Name: "Reproit"})
	if err != nil {
		panic(err)
	}
	response, err := protojson.Marshal(reply)
	if err != nil {
		panic(err)
	}
	responsePath := filepath.Join(output, "grpc-helloworld-response.json")
	if err := os.WriteFile(responsePath, response, 0o600); err != nil {
		panic(err)
	}
	set := &descriptorpb.FileDescriptorSet{File: []*descriptorpb.FileDescriptorProto{
		protodesc.ToFileDescriptorProto(pb.File_examples_helloworld_helloworld_helloworld_proto),
	}}
	descriptor, err := protojson.Marshal(set)
	if err != nil {
		panic(err)
	}
	descriptorPath := filepath.Join(output, "grpc-helloworld-descriptor.json")
	if err := os.WriteFile(descriptorPath, descriptor, 0o600); err != nil {
		panic(err)
	}
	fmt.Println(string(response))
}
